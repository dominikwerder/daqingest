use super::conn::{CaConnEvent, ConnCommand};
use super::store::DataStore;
use super::IngestCommons;
use crate::ca::conn::CaConn;
use crate::errconv::ErrConv;
use crate::rt::{JoinHandle, TokMx};
use crate::store::CommonInsertItemQueueSender;
use async_channel::{Receiver, Sender};
use err::Error;
use futures_util::{FutureExt, StreamExt};
use netpod::log::*;
use stats::CaConnStats;
use std::collections::{BTreeMap, VecDeque};
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

pub struct CommandQueueSet {
    queues: TokMx<BTreeMap<SocketAddrV4, Sender<ConnCommand>>>,
}

impl CommandQueueSet {
    pub fn new() -> Self {
        Self {
            queues: TokMx::new(BTreeMap::<SocketAddrV4, Sender<ConnCommand>>::new()),
        }
    }

    pub async fn queues(&self) -> &TokMx<BTreeMap<SocketAddrV4, Sender<ConnCommand>>> {
        &self.queues
    }

    pub async fn queues_locked(&self) -> tokio::sync::MutexGuard<BTreeMap<SocketAddrV4, Sender<ConnCommand>>> {
        let mut g = self.queues.lock().await;
        let mut rm = Vec::new();
        for (k, v) in g.iter() {
            if v.is_closed() {
                rm.push(*k);
            }
        }
        for x in rm {
            g.remove(&x);
        }
        g
    }
}

pub struct CaConnRess {
    sender: Sender<ConnCommand>,
    stats: Arc<CaConnStats>,
    jh: JoinHandle<Result<(), Error>>,
}

impl CaConnRess {
    pub fn stats(&self) -> &Arc<CaConnStats> {
        &self.stats
    }
}

// TODO
// Resources belonging to the same CaConn also belong together here.
// Only add or remove them from the set at once.
// That means, they should go together.
// Does not hold the actual CaConn, because that struct is in a task.
// Always create the CaConn via a common code path which also takes care
// to add it to the correct list.
// There, make spawning part of this function?
pub struct CaConnSet {
    ca_conn_ress: TokMx<BTreeMap<SocketAddr, CaConnRess>>,
    conn_item_tx: Sender<CaConnEvent>,
    conn_item_rx: Receiver<CaConnEvent>,
}

impl CaConnSet {
    pub fn new() -> Self {
        let (conn_item_tx, conn_item_rx) = async_channel::bounded(10000);
        Self {
            ca_conn_ress: Default::default(),
            conn_item_tx,
            conn_item_rx,
        }
    }

    pub fn conn_item_rx(&self) -> Receiver<CaConnEvent> {
        self.conn_item_rx.clone()
    }

    pub fn ca_conn_ress(&self) -> &TokMx<BTreeMap<SocketAddr, CaConnRess>> {
        &self.ca_conn_ress
    }

    pub async fn create_ca_conn(
        &self,
        backend: String,
        addr: SocketAddrV4,
        local_epics_hostname: String,
        array_truncate: usize,
        insert_queue_max: usize,
        insert_item_queue_sender: CommonInsertItemQueueSender,
        data_store: Arc<DataStore>,
        with_channels: Vec<String>,
    ) -> Result<(), Error> {
        info!("create new CaConn  {:?}", addr);
        let addr2 = SocketAddr::V4(addr.clone());
        let mut conn = CaConn::new(
            backend.clone(),
            addr,
            local_epics_hostname,
            data_store.clone(),
            insert_item_queue_sender,
            array_truncate,
            insert_queue_max,
        );
        for ch in with_channels {
            conn.channel_add(ch);
        }
        let conn = conn;
        let conn_tx = conn.conn_command_tx();
        let conn_stats = conn.stats();
        let conn_item_tx = self.conn_item_tx.clone();
        let conn_fut = async move {
            let stats = conn.stats();
            let mut conn = conn;
            while let Some(item) = conn.next().await {
                match item {
                    Ok(item) => {
                        stats.conn_item_count_inc();
                        conn_item_tx.send(item).await?;
                    }
                    Err(e) => {
                        error!("CaConn gives error: {e:?}");
                        return Err(e);
                    }
                }
            }
            Ok(())
        };
        let jh = tokio::spawn(conn_fut);
        let ca_conn_ress = CaConnRess {
            sender: conn_tx,
            stats: conn_stats,
            jh,
        };
        self.ca_conn_ress.lock().await.insert(addr2, ca_conn_ress);
        Ok(())
    }

    pub async fn send_command_to_all<F, R>(&self, cmdgen: F) -> Result<Vec<R>, Error>
    where
        F: Fn() -> (ConnCommand, async_channel::Receiver<R>),
    {
        //let it = self.ca_conn_ress.iter().map(|x| x);
        //Self::send_command_inner(it, move || cmd.clone());
        let mut rxs = Vec::new();
        for (_addr, ress) in &*self.ca_conn_ress.lock().await {
            let (cmd, rx) = cmdgen();
            match ress.sender.send(cmd).await {
                Ok(()) => {
                    rxs.push(rx);
                }
                Err(e) => {
                    error!("can not send command {e:?}");
                }
            }
        }
        let mut res = Vec::new();
        for rx in rxs {
            let x = rx.recv().await?;
            res.push(x);
        }
        Ok(res)
    }

    pub async fn send_command_to_addr<F, R>(&self, addr: &SocketAddr, cmdgen: F) -> Result<R, Error>
    where
        F: Fn() -> (ConnCommand, async_channel::Receiver<R>),
    {
        if let Some(ress) = self.ca_conn_ress.lock().await.get(addr) {
            let (cmd, rx) = cmdgen();
            ress.sender.send(cmd).await.err_conv()?;
            let ret = rx.recv().await.err_conv()?;
            Ok(ret)
        } else {
            Err(Error::with_msg_no_trace(format!("addr not found")))
        }
    }

    #[allow(unused)]
    async fn send_command_inner<'a, IT, F, R>(it: &mut IT, cmdgen: F) -> Vec<async_channel::Receiver<R>>
    where
        IT: Iterator<Item = (&'a SocketAddrV4, &'a async_channel::Sender<ConnCommand>)>,
        F: Fn() -> (ConnCommand, async_channel::Receiver<R>),
    {
        let mut rxs = Vec::new();
        for (_, tx) in it {
            let (cmd, rx) = cmdgen();
            match tx.send(cmd).await {
                Ok(()) => {
                    rxs.push(rx);
                }
                Err(e) => {
                    error!("can not send command {e:?}");
                }
            }
        }
        rxs
    }

    pub async fn send_stop(&self) -> Result<(), Error> {
        self.send_command_to_all(|| ConnCommand::shutdown()).await?;
        Ok(())
    }

    pub async fn wait_stopped(&self) -> Result<(), Error> {
        let mut g = self.ca_conn_ress.lock().await;
        let mm = std::mem::replace(&mut *g, BTreeMap::new());
        let mut jhs: VecDeque<_> = VecDeque::new();
        for t in mm {
            jhs.push_back(t.1.jh.fuse());
        }
        loop {
            let mut jh = if let Some(x) = jhs.pop_front() {
                x
            } else {
                break;
            };
            futures_util::select! {
                a = jh => match a {
                    Ok(k) => match k {
                        Ok(_) => {}
                        Err(e) => {
                            error!("{e:?}");
                        }
                    },
                    Err(e) => {
                        error!("{e:?}");
                    }
                },
                _b = crate::rt::sleep(Duration::from_millis(1000)).fuse() => {
                    jhs.push_back(jh);
                    info!("waiting for {} connections", jhs.len());
                }
            };
        }
        Ok(())
    }

    /// Add channel, or create a new CaConn and add the channel.
    pub async fn add_channel_to_addr(
        &self,
        backend: String,
        addr: SocketAddr,
        channel_name: String,
        ingest_commons: Arc<IngestCommons>,
    ) -> Result<(), Error> {
        let g = self.ca_conn_ress.lock().await;
        match g.get(&addr) {
            Some(ca_conn) => {
                //info!("try to add to existing... {addr} {channel_name}");
                let (cmd, rx) = ConnCommand::channel_add(channel_name);
                ca_conn.sender.send(cmd).await.err_conv()?;
                let a = rx.recv().await.err_conv()?;
                if a {
                    Ok(())
                } else {
                    Err(Error::with_msg_no_trace(format!("channel add failed")))
                }
            }
            None => {
                //info!("create new {addr} {channel_name}");
                drop(g);
                let addr = if let SocketAddr::V4(x) = addr {
                    x
                } else {
                    return Err(Error::with_msg_no_trace(format!("only ipv4 supported for IOC")));
                };
                // TODO use parameters:
                self.create_ca_conn(
                    backend.clone(),
                    addr,
                    ingest_commons.local_epics_hostname.clone(),
                    512,
                    200,
                    ingest_commons.insert_item_queue.sender().await,
                    ingest_commons.data_store.clone(),
                    vec![channel_name],
                )
                .await?;
                Ok(())
            }
        }
    }

    pub async fn has_addr(&self, addr: &SocketAddr) -> bool {
        // TODO only used to check on add-channel whether we want to add channel to conn, or create new conn.
        // TODO must do that atomic.
        self.ca_conn_ress.lock().await.contains_key(addr)
    }

    pub async fn addr_nth_mod(&self, n: usize) -> Option<SocketAddr> {
        let g = self.ca_conn_ress.lock().await;
        let len = g.len();
        if len < 1 {
            return None;
        }
        let n = n % len;
        g.keys().take(n).last().map(Clone::clone)
    }
}
