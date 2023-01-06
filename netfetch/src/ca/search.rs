use crate::ca::findioc::FindIocStream;
use crate::conf::CaIngestOpts;
use err::Error;
use futures_util::StreamExt;
use log::*;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn resolve_address(addr_str: &str) -> Result<SocketAddr, Error> {
    const PORT_DEFAULT: u16 = 5064;
    let ac = match addr_str.parse::<SocketAddr>() {
        Ok(k) => k,
        Err(_) => {
            trace!("can not parse {addr_str} as SocketAddr");
            match addr_str.parse::<IpAddr>() {
                Ok(k) => SocketAddr::new(k, PORT_DEFAULT),
                Err(_e) => {
                    trace!("can not parse {addr_str} as IpAddr");
                    let (hostname, port) = if addr_str.contains(":") {
                        let mut it = addr_str.split(":");
                        (
                            it.next().unwrap().to_string(),
                            it.next().unwrap().parse::<u16>().unwrap(),
                        )
                    } else {
                        (addr_str.to_string(), PORT_DEFAULT)
                    };
                    let host = format!("{}:{}", hostname.clone(), port);
                    match tokio::net::lookup_host(host.clone()).await {
                        Ok(mut k) => {
                            if let Some(k) = k.next() {
                                k
                            } else {
                                return Err(Error::with_msg_no_trace(format!("can not lookup host {host}")));
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    };
    Ok(ac)
}

pub async fn ca_search(opts: CaIngestOpts, channels: &Vec<String>) -> Result<(), Error> {
    info!("ca_search begin");
    let d = opts.postgresql().clone();
    let (pg_client, pg_conn) = tokio_postgres::connect(
        &format!("postgresql://{}:{}@{}:{}/{}", d.user, d.pass, d.host, d.port, d.name),
        tokio_postgres::tls::NoTls,
    )
    .await
    .unwrap();
    // TODO join pg_conn in the end:
    tokio::spawn(pg_conn);
    let pg_client = Arc::new(pg_client);
    let qu_insert = {
        const TEXT: tokio_postgres::types::Type = tokio_postgres::types::Type::TEXT;
        pg_client
            .prepare_typed(
                "insert into ioc_by_channel_log (facility, channel, responseaddr, addr) values ($1, $2, $3, $4)",
                &[TEXT, TEXT, TEXT, TEXT],
            )
            .await
            .unwrap()
    };
    let mut addrs = Vec::new();
    for s in opts.search() {
        match resolve_address(s).await {
            Ok(addr) => {
                info!("resolved {s} as {addr}");
                addrs.push(addr);
            }
            Err(e) => {
                error!("can not resolve {s} {e}");
            }
        }
    }
    let gw_addrs = {
        let mut gw_addrs = Vec::new();
        for s in opts.search_blacklist() {
            match resolve_address(s).await {
                Ok(addr) => {
                    info!("resolved {s} as {addr}");
                    gw_addrs.push(addr);
                }
                Err(e) => {
                    error!("can not resolve {s} {e}");
                }
            }
        }
        gw_addrs
    };
    info!("Blacklisting {} gateways", gw_addrs.len());
    let addrs = addrs
        .into_iter()
        .filter_map(|x| match x {
            SocketAddr::V4(x) => Some(x),
            SocketAddr::V6(_) => {
                error!("TODO check ipv6 support for IOCs");
                None
            }
        })
        .collect();
    let mut finder = FindIocStream::new(addrs, Duration::from_millis(1000), 20, 1);
    for ch in channels.iter() {
        finder.push(ch.into());
    }
    let mut ts_last = Instant::now();
    loop {
        let ts_now = Instant::now();
        if ts_now.duration_since(ts_last) >= Duration::from_millis(1000) {
            ts_last = ts_now;
            info!("{}", finder.quick_state());
        }
        let k = tokio::time::timeout(Duration::from_millis(1500), finder.next()).await;
        let item = match k {
            Ok(Some(k)) => k,
            Ok(None) => {
                info!("Search stream exhausted");
                break;
            }
            Err(_) => {
                continue;
            }
        };
        let item = match item {
            Ok(k) => k,
            Err(e) => {
                error!("ca_search {e:?}");
                continue;
            }
        };
        for item in item {
            let mut do_block = false;
            for a2 in &gw_addrs {
                if let Some(response_addr) = &item.response_addr {
                    if &SocketAddr::V4(*response_addr) == a2 {
                        do_block = true;
                        warn!("gateways responded to search");
                    }
                }
            }
            if let Some(a1) = item.addr.as_ref() {
                for a2 in &gw_addrs {
                    if &SocketAddr::V4(*a1) == a2 {
                        do_block = true;
                        warn!("do not use gateways as ioc address");
                    }
                }
            }
            if do_block {
                info!("blacklisting {item:?}");
            } else {
                let responseaddr = item.response_addr.map(|x| x.to_string());
                let addr = item.addr.map(|x| x.to_string());
                pg_client
                    .execute(&qu_insert, &[&opts.backend(), &item.channel, &responseaddr, &addr])
                    .await
                    .unwrap();
            }
        }
    }
    Ok(())
}
