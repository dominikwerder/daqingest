use crate::zmtp::ErrConv;
use crate::zmtp::{CommonQueries, ZmtpFrame};
use err::Error;
use futures_core::Future;
use futures_util::FutureExt;
use log::*;
use netpod::timeunits::SEC;
use netpod::{ByteOrder, ScalarType, Shape};
use scylla::batch::{Batch, BatchType};
use scylla::frame::value::{BatchValues, ValueList};
use scylla::prepared_statement::PreparedStatement;
use scylla::transport::errors::QueryError;
use scylla::{BatchResult, QueryResult, Session as ScySession};
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

pub struct ScyQueryFut<V> {
    #[allow(unused)]
    scy: Arc<ScySession>,
    #[allow(unused)]
    query: Box<PreparedStatement>,
    #[allow(unused)]
    values: Box<V>,
    fut: Pin<Box<dyn Future<Output = Result<QueryResult, QueryError>>>>,
}

impl<V> ScyQueryFut<V> {
    pub fn new(scy: Arc<ScySession>, query: PreparedStatement, values: V) -> Self
    where
        V: ValueList + 'static,
    {
        let query = Box::new(query);
        let values = Box::new(values);
        let scy2 = unsafe { &*(&scy as &_ as *const _) } as &ScySession;
        let query2 = unsafe { &*(&query as &_ as *const _) } as &PreparedStatement;
        let v2 = unsafe { &*(&values as &_ as *const _) } as &V;
        let fut = scy2.execute(query2, v2);
        Self {
            scy,
            query,
            values,
            fut: Box::pin(fut),
        }
    }
}

impl<V> Future for ScyQueryFut<V> {
    type Output = Result<(), Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        use Poll::*;
        match self.fut.poll_unpin(cx) {
            Ready(k) => match k {
                Ok(_) => {
                    info!("ScyQueryFut done Ok");
                    Ready(Ok(()))
                }
                Err(e) => {
                    warn!("ScyQueryFut done Err");
                    Ready(Err(e).err_conv())
                }
            },
            Pending => Pending,
        }
    }
}

pub struct ScyBatchFut<V> {
    #[allow(unused)]
    scy: Arc<ScySession>,
    #[allow(unused)]
    batch: Box<Batch>,
    #[allow(unused)]
    values: Box<V>,
    fut: Pin<Box<dyn Future<Output = Result<BatchResult, QueryError>>>>,
    polled: usize,
    ts_create: Instant,
    ts_poll_start: Instant,
}

impl<V> ScyBatchFut<V> {
    pub fn new(scy: Arc<ScySession>, batch: Batch, values: V) -> Self
    where
        V: BatchValues + 'static,
    {
        let batch = Box::new(batch);
        let values = Box::new(values);
        let scy2 = unsafe { &*(&scy as &_ as *const _) } as &ScySession;
        let batch2 = unsafe { &*(&batch as &_ as *const _) } as &Batch;
        let v2 = unsafe { &*(&values as &_ as *const _) } as &V;
        let fut = scy2.batch(batch2, v2);
        let tsnow = Instant::now();
        Self {
            scy,
            batch,
            values,
            fut: Box::pin(fut),
            polled: 0,
            ts_create: tsnow,
            ts_poll_start: tsnow,
        }
    }
}

impl<V> Future for ScyBatchFut<V> {
    type Output = Result<(), Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        use Poll::*;
        if self.polled == 0 {
            self.ts_poll_start = Instant::now();
        }
        self.polled += 1;
        match self.fut.poll_unpin(cx) {
            Ready(k) => match k {
                Ok(_) => {
                    trace!("ScyBatchFut done Ok");
                    Ready(Ok(()))
                }
                Err(e) => {
                    let tsnow = Instant::now();
                    let dt_created = tsnow.duration_since(self.ts_create).as_secs_f32() * 1e3;
                    let dt_polled = tsnow.duration_since(self.ts_poll_start).as_secs_f32() * 1e3;
                    warn!(
                        "ScyBatchFut  polled {}  dt_created {:6.2} ms  dt_polled {:6.2} ms",
                        self.polled, dt_created, dt_polled
                    );
                    warn!("ScyBatchFut done Err  {e:?}");
                    Ready(Err(e).err_conv())
                }
            },
            Pending => Pending,
        }
    }
}

pub struct ScyBatchFutGen {
    #[allow(unused)]
    scy: Arc<ScySession>,
    #[allow(unused)]
    batch: Box<Batch>,
    fut: Pin<Box<dyn Future<Output = Result<BatchResult, QueryError>>>>,
    polled: usize,
    ts_create: Instant,
    ts_poll_start: Instant,
}

impl ScyBatchFutGen {
    pub fn new<V>(scy: Arc<ScySession>, batch: Batch, values: V) -> Self
    where
        V: BatchValues + 'static,
    {
        let batch = Box::new(batch);
        let scy_ref = unsafe { &*(&scy as &_ as *const _) } as &ScySession;
        let batch_ref = unsafe { &*(&batch as &_ as *const _) } as &Batch;
        let fut = scy_ref.batch(batch_ref, values);
        let tsnow = Instant::now();
        Self {
            scy,
            batch,
            fut: Box::pin(fut),
            polled: 0,
            ts_create: tsnow,
            ts_poll_start: tsnow,
        }
    }
}

impl Future for ScyBatchFutGen {
    type Output = Result<(), Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        use Poll::*;
        if self.polled == 0 {
            self.ts_poll_start = Instant::now();
        }
        self.polled += 1;
        match self.fut.poll_unpin(cx) {
            Ready(k) => match k {
                Ok(_) => {
                    trace!("ScyBatchFutGen done Ok");
                    Ready(Ok(()))
                }
                Err(e) => {
                    let tsnow = Instant::now();
                    let dt_created = tsnow.duration_since(self.ts_create).as_secs_f32() * 1e3;
                    let dt_polled = tsnow.duration_since(self.ts_poll_start).as_secs_f32() * 1e3;
                    warn!(
                        "ScyBatchFutGen  polled {}  dt_created {:6.2} ms  dt_polled {:6.2} ms",
                        self.polled, dt_created, dt_polled
                    );
                    warn!("ScyBatchFutGen done Err  {e:?}");
                    Ready(Err(e).err_conv())
                }
            },
            Pending => Pending,
        }
    }
}

pub struct ChannelWriteRes {
    pub nrows: u32,
    pub dt: Duration,
}

pub struct ChannelWriteFut {
    nn: usize,
    fut1: Option<Pin<Box<dyn Future<Output = Result<(), Error>>>>>,
    fut2: Option<Pin<Box<dyn Future<Output = Result<(), Error>>>>>,
    ts1: Option<Instant>,
    mask: u8,
}

impl Future for ChannelWriteFut {
    type Output = Result<ChannelWriteRes, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        use Poll::*;
        loop {
            break if self.ts1.is_none() {
                self.ts1 = Some(Instant::now());
                continue;
            } else if let Some(f) = self.fut1.as_mut() {
                match f.poll_unpin(cx) {
                    Ready(k) => {
                        trace!("ChannelWriteFut  fut1 Ready");
                        self.fut1 = None;
                        self.mask |= 1;
                        match k {
                            Ok(_) => continue,
                            Err(e) => Ready(Err(e)),
                        }
                    }
                    Pending => Pending,
                }
            } else if let Some(f) = self.fut2.as_mut() {
                match f.poll_unpin(cx) {
                    Ready(k) => {
                        trace!("ChannelWriteFut  fut2 Ready");
                        self.fut2 = None;
                        self.mask |= 2;
                        match k {
                            Ok(_) => continue,
                            Err(e) => Ready(Err(e)),
                        }
                    }
                    Pending => Pending,
                }
            } else {
                if self.mask != 0 {
                    let ts2 = Instant::now();
                    let dt = ts2.duration_since(self.ts1.unwrap());
                    if false {
                        trace!(
                            "ChannelWriteFut  inserted  nn {}  dt {:6.2} ms",
                            self.nn,
                            dt.as_secs_f32() * 1e3
                        );
                    }
                    let res = ChannelWriteRes {
                        nrows: self.nn as u32,
                        dt,
                    };
                    Ready(Ok(res))
                } else {
                    let res = ChannelWriteRes {
                        nrows: 0,
                        dt: Duration::from_millis(0),
                    };
                    Ready(Ok(res))
                }
            };
        }
    }
}

pub trait ChannelWriter {
    fn write_msg(&mut self, ts: u64, pulse: u64, fr: &ZmtpFrame) -> Result<ChannelWriteFut, Error>;
}

trait MsgAcceptor {
    fn len(&self) -> usize;
    fn accept(&mut self, ts_msp: i64, ts_lsp: i64, pulse: i64, fr: &ZmtpFrame) -> Result<(), Error>;
    fn should_flush(&self) -> bool;
    fn flush_batch(&mut self, scy: Arc<ScySession>) -> Result<ScyBatchFutGen, Error>;
}

macro_rules! impl_msg_acceptor_scalar {
    ($sname:ident, $st:ty, $qu_id:ident, $from_bytes:ident) => {
        struct $sname {
            query: PreparedStatement,
            values: Vec<(i32, i64, i64, i64, $st)>,
            series: i32,
        }

        impl $sname {
            #[allow(unused)]
            pub fn new(series: i32, cq: &CommonQueries) -> Self {
                Self {
                    query: cq.$qu_id.clone(),
                    values: vec![],
                    series,
                }
            }
        }

        impl MsgAcceptor for $sname {
            fn len(&self) -> usize {
                self.values.len()
            }

            fn accept(&mut self, ts_msp: i64, ts_lsp: i64, pulse: i64, fr: &ZmtpFrame) -> Result<(), Error> {
                type ST = $st;
                const STL: usize = std::mem::size_of::<ST>();
                let value = ST::$from_bytes(fr.data()[0..STL].try_into()?);
                self.values.push((self.series, ts_msp, ts_lsp, pulse, value));
                Ok(())
            }

            fn should_flush(&self) -> bool {
                self.len() >= 140 + ((self.series as usize) & 0x1f)
            }

            fn flush_batch(&mut self, scy: Arc<ScySession>) -> Result<ScyBatchFutGen, Error> {
                let vt = mem::replace(&mut self.values, vec![]);
                let nn = vt.len();
                let mut batch = Batch::new(BatchType::Unlogged);
                for _ in 0..nn {
                    batch.append_statement(self.query.clone());
                }
                let ret = ScyBatchFutGen::new(scy, batch, vt);
                Ok(ret)
            }
        }
    };
}

macro_rules! impl_msg_acceptor_array {
    ($sname:ident, $st:ty, $qu_id:ident, $from_bytes:ident) => {
        struct $sname {
            query: PreparedStatement,
            values: Vec<(i32, i64, i64, i64, Vec<$st>)>,
            series: i32,
            array_truncate: usize,
        }

        impl $sname {
            #[allow(unused)]
            pub fn new(series: i32, array_truncate: usize, cq: &CommonQueries) -> Self {
                Self {
                    query: cq.$qu_id.clone(),
                    values: vec![],
                    series,
                    array_truncate,
                }
            }
        }

        impl MsgAcceptor for $sname {
            fn len(&self) -> usize {
                self.values.len()
            }

            fn accept(&mut self, ts_msp: i64, ts_lsp: i64, pulse: i64, fr: &ZmtpFrame) -> Result<(), Error> {
                type ST = $st;
                const STL: usize = std::mem::size_of::<ST>();
                let vc = fr.data().len() / STL;
                let mut values = Vec::with_capacity(vc);
                for i in 0..vc {
                    let h = i * STL;
                    let value = ST::$from_bytes(fr.data()[h..h + STL].try_into()?);
                    values.push(value);
                }
                values.truncate(self.array_truncate);
                self.values.push((self.series, ts_msp, ts_lsp, pulse, values));
                Ok(())
            }

            fn should_flush(&self) -> bool {
                self.len() >= 40 + ((self.series as usize) & 0x7)
            }

            fn flush_batch(&mut self, scy: Arc<ScySession>) -> Result<ScyBatchFutGen, Error> {
                let vt = mem::replace(&mut self.values, vec![]);
                let nn = vt.len();
                let mut batch = Batch::new(BatchType::Unlogged);
                for _ in 0..nn {
                    batch.append_statement(self.query.clone());
                }
                let ret = ScyBatchFutGen::new(scy, batch, vt);
                Ok(ret)
            }
        }
    };
}

impl_msg_acceptor_scalar!(MsgAcceptorScalarU16LE, i16, qu_insert_scalar_i16, from_le_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarU16BE, i16, qu_insert_scalar_i16, from_be_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarU32LE, i32, qu_insert_scalar_i32, from_le_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarU32BE, i32, qu_insert_scalar_i32, from_be_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarI16LE, i16, qu_insert_scalar_i16, from_le_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarI16BE, i16, qu_insert_scalar_i16, from_be_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarF32LE, f32, qu_insert_scalar_f32, from_le_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarF32BE, f32, qu_insert_scalar_f32, from_be_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarF64LE, f64, qu_insert_scalar_f64, from_le_bytes);
impl_msg_acceptor_scalar!(MsgAcceptorScalarF64BE, f64, qu_insert_scalar_f64, from_be_bytes);

impl_msg_acceptor_array!(MsgAcceptorArrayU16LE, i16, qu_insert_array_u16, from_le_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayU16BE, i16, qu_insert_array_u16, from_be_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayI16LE, i16, qu_insert_array_i16, from_le_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayI16BE, i16, qu_insert_array_i16, from_be_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayF32LE, f32, qu_insert_array_f32, from_le_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayF32BE, f32, qu_insert_array_f32, from_be_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayF64LE, f64, qu_insert_array_f64, from_le_bytes);
impl_msg_acceptor_array!(MsgAcceptorArrayF64BE, f64, qu_insert_array_f64, from_be_bytes);

pub struct ChannelWriterAll {
    series: u32,
    scy: Arc<ScySession>,
    common_queries: Arc<CommonQueries>,
    ts_msp_lsp: fn(u64, u32) -> (u64, u64),
    ts_msp_last: u64,
    acceptor: Box<dyn MsgAcceptor>,
    dtype_mark: u32,
}

impl ChannelWriterAll {
    pub fn new(
        series: u32,
        common_queries: Arc<CommonQueries>,
        scy: Arc<ScySession>,
        scalar_type: ScalarType,
        shape: Shape,
        byte_order: ByteOrder,
        array_truncate: usize,
    ) -> Result<Self, Error> {
        let dtype_mark = scalar_type.index() as u32;
        let dtype_mark = match &shape {
            Shape::Scalar => dtype_mark,
            Shape::Wave(_) => 1000 + dtype_mark,
            Shape::Image(_, _) => 2000 + dtype_mark,
        };
        let (ts_msp_lsp, acc): (fn(u64, u32) -> (u64, u64), Box<dyn MsgAcceptor>) = match &shape {
            Shape::Scalar => match &scalar_type {
                ScalarType::U16 => match &byte_order {
                    ByteOrder::BE => {
                        let acc = MsgAcceptorScalarU16BE::new(series as i32, &common_queries);
                        (ts_msp_lsp_1, Box::new(acc) as _)
                    }
                    ByteOrder::LE => {
                        return Err(Error::with_msg_no_trace(format!(
                            "TODO  {:?}  {:?}  {:?}",
                            scalar_type, shape, byte_order
                        )));
                    }
                },
                ScalarType::U32 => match &byte_order {
                    ByteOrder::BE => {
                        let acc = MsgAcceptorScalarU32BE::new(series as i32, &common_queries);
                        (ts_msp_lsp_1, Box::new(acc) as _)
                    }
                    ByteOrder::LE => {
                        return Err(Error::with_msg_no_trace(format!(
                            "TODO  {:?}  {:?}  {:?}",
                            scalar_type, shape, byte_order
                        )));
                    }
                },
                ScalarType::F32 => match &byte_order {
                    ByteOrder::BE => {
                        let acc = MsgAcceptorScalarF32BE::new(series as i32, &common_queries);
                        (ts_msp_lsp_1, Box::new(acc) as _)
                    }
                    ByteOrder::LE => {
                        return Err(Error::with_msg_no_trace(format!(
                            "TODO  {:?}  {:?}  {:?}",
                            scalar_type, shape, byte_order
                        )));
                    }
                },
                ScalarType::F64 => match &byte_order {
                    ByteOrder::BE => {
                        let acc = MsgAcceptorScalarF64BE::new(series as i32, &common_queries);
                        (ts_msp_lsp_1, Box::new(acc) as _)
                    }
                    ByteOrder::LE => {
                        return Err(Error::with_msg_no_trace(format!(
                            "TODO  {:?}  {:?}  {:?}",
                            scalar_type, shape, byte_order
                        )));
                    }
                },
                _ => {
                    return Err(Error::with_msg_no_trace(format!(
                        "TODO  {:?}  {:?}  {:?}",
                        scalar_type, shape, byte_order
                    )));
                }
            },
            Shape::Wave(nele) => {
                info!("set up wave acceptor  nele {nele}");
                match &scalar_type {
                    ScalarType::U16 => match &byte_order {
                        ByteOrder::LE => {
                            let acc = MsgAcceptorArrayU16LE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                        ByteOrder::BE => {
                            let acc = MsgAcceptorArrayU16BE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                    },
                    ScalarType::I16 => match &byte_order {
                        ByteOrder::LE => {
                            let acc = MsgAcceptorArrayI16LE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                        ByteOrder::BE => {
                            let acc = MsgAcceptorArrayI16BE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                    },
                    ScalarType::F32 => match &byte_order {
                        ByteOrder::LE => {
                            let acc = MsgAcceptorArrayF32LE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                        ByteOrder::BE => {
                            let acc = MsgAcceptorArrayF32BE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                    },
                    ScalarType::F64 => match &byte_order {
                        ByteOrder::LE => {
                            let acc = MsgAcceptorArrayF64LE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                        ByteOrder::BE => {
                            let acc = MsgAcceptorArrayF64BE::new(series as i32, array_truncate, &common_queries);
                            (ts_msp_lsp_2, Box::new(acc) as _)
                        }
                    },
                    _ => {
                        return Err(Error::with_msg_no_trace(format!(
                            "TODO  {:?}  {:?}  {:?}",
                            scalar_type, shape, byte_order
                        )));
                    }
                }
            }
            _ => {
                return Err(Error::with_msg_no_trace(format!(
                    "TODO  {:?}  {:?}  {:?}",
                    scalar_type, shape, byte_order
                )));
            }
        };
        let ret = Self {
            series,
            scy,
            common_queries,
            ts_msp_lsp,
            ts_msp_last: 0,
            acceptor: acc,
            dtype_mark,
        };
        Ok(ret)
    }

    pub fn dtype_mark(&self) -> u32 {
        self.dtype_mark
    }

    pub fn write_msg_impl(&mut self, ts: u64, pulse: u64, fr: &ZmtpFrame) -> Result<ChannelWriteFut, Error> {
        let (ts_msp, ts_lsp) = (self.ts_msp_lsp)(ts, self.series);
        let fut1 = if ts_msp != self.ts_msp_last {
            info!("ts_msp changed  ts {ts}  pulse {pulse}  ts_msp {ts_msp}  ts_lsp {ts_lsp}");
            self.ts_msp_last = ts_msp;
            // TODO make the passing of the query parameters type safe.
            let fut = ScyQueryFut::new(
                self.scy.clone(),
                self.common_queries.qu_insert_ts_msp.clone(),
                (self.series as i32, ts_msp as i64, self.dtype_mark as i32),
            );
            Some(Box::pin(fut) as _)
        } else {
            None
        };
        self.acceptor.accept(ts_msp as i64, ts_lsp as i64, pulse as i64, fr)?;
        if self.acceptor.should_flush() {
            let nn = self.acceptor.len();
            let fut = self.acceptor.flush_batch(self.scy.clone())?;
            let fut2 = Some(Box::pin(fut) as _);
            let ret = ChannelWriteFut {
                ts1: None,
                mask: 0,
                nn,
                fut1,
                fut2,
            };
            Ok(ret)
        } else {
            let ret = ChannelWriteFut {
                ts1: None,
                mask: 0,
                nn: 0,
                fut1: fut1,
                fut2: None,
            };
            Ok(ret)
        }
    }
}

impl ChannelWriter for ChannelWriterAll {
    fn write_msg(&mut self, ts: u64, pulse: u64, fr: &ZmtpFrame) -> Result<ChannelWriteFut, Error> {
        self.write_msg_impl(ts, pulse, fr)
    }
}

fn ts_msp_lsp_1(ts: u64, series: u32) -> (u64, u64) {
    ts_msp_lsp_gen(ts, series, 100 * SEC)
}

fn ts_msp_lsp_2(ts: u64, series: u32) -> (u64, u64) {
    ts_msp_lsp_gen(ts, series, 10 * SEC)
}

fn ts_msp_lsp_gen(ts: u64, series: u32, fak: u64) -> (u64, u64) {
    if ts < u32::MAX as u64 {
        return (0, 0);
    }
    let off = series as u64;
    let ts_a = ts - off;
    let ts_b = ts_a / fak;
    let ts_lsp = ts_a % fak;
    let ts_msp = ts_b * fak + off;
    (ts_msp, ts_lsp)
}
