use std::sync::mpsc;

use evtloop::{Protocol, ConnectionRef, ConnectionHandle};

pub struct Simple;
#[derive(Debug)]
pub struct Msg;

impl Protocol for Simple {
    type Message = Msg;

    fn new(_: ConnectionHandle<Self>) -> Simple {
        Simple
    }

    fn on_data(&mut self, buf: &[u8], _conn: ConnectionRef<Simple>) -> Result<usize, ()> {
        trace!("Simple: Received sth");
        Ok(buf.len())
    }

    fn ready_write(&mut self, mut conn: ConnectionRef<Simple>) {
        trace!("Simple: Socket ready for writing");
    }

    fn notify(&mut self, msg: Msg, mut conn: ConnectionRef<Simple>) {
        trace!("Simple: Notified: msg={:?}", msg);
    }
}
