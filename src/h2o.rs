use std::sync::mpsc;

use evtloop::{Protocol, ConnectionRef};

pub struct Http2 {
    streams: Vec<ProtoStream>,
    init: bool,
}

impl Protocol for Http2 {
    type Message = HttpMsg;

    fn new(_: ConnectionRef<Http2>) -> Http2 {
        Http2 {
            streams: Vec::new(),
            init: false,
        }
    }

    fn on_data(&mut self, buf: &[u8], _conn: ConnectionRef<Http2>) -> Result<usize, ()> {
        trace!("Http2: Received something back");
        self.streams[0].tx.send(buf.into()).unwrap();
        Ok(buf.len())
    }

    fn ready_write(&mut self, mut conn: ConnectionRef<Http2>) {
        // TODO See about giving it only a reference to some parts of the connection
        // (perhaps conveniently wrapped in some helper wrapper) instead of the
        // full Conn. In fact, that is probably a must, as the protocol would like
        // to have a reference to the event loop too, which the Connection currently
        // does not and should not have (as it is passed as a parameter). The proto
        // could use the ref to the evtloop so that it can dispatch messages to it,
        // perhaps even asynchronously.
        trace!("Hello, from HTTP2");
        if !self.init {
            conn.queue_frame((&b"Hello from Http2"[..]).into());
            self.init = true;
        } else {
            for stream in self.streams.iter() {
                stream.rx.try_recv().map(|buf| conn.queue_frame(buf));
            }
        }
    }

    fn notify(&mut self, msg: HttpMsg, mut conn: ConnectionRef<Http2>) {
        trace!("Http2 notified: msg={:?}", msg);
        match msg {
            HttpMsg::NewStream(resolve) => {
                let (out_tx, out_rx) = mpsc::channel();
                let (in_tx, in_rx) = mpsc::channel();
                let (stream, proto_stream) = (Stream::new(out_tx, in_rx), ProtoStream::new(out_rx, in_tx));
                self.streams.push(proto_stream);
                conn.queue_frame((&b"New Stream Header"[..]).into());
                resolve.send(stream).unwrap();
            },
        }
    }
}

pub enum HttpMsg {
    NewStream(mpsc::Sender<Stream>),
}

impl ::std::fmt::Debug for HttpMsg {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        f.debug_struct("HttpMsg").finish()
    }
}

/// The client end of the stream. Allows them to queue data for writing and handle data that was
/// meant for that stream.
pub struct Stream {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl Stream {
    fn new(tx: mpsc::Sender<Vec<u8>>, rx: mpsc::Receiver<Vec<u8>>) -> Stream {
        Stream {
            tx: tx,
            rx: rx,
        }
    }

    pub fn write<B: Into<Vec<u8>>>(&self, buf: B) {
        self.tx.send(buf.into()).unwrap();
    }

    pub fn recv_chunk(&self) -> Result<Vec<u8>, ()> {
        self.rx.recv().map_err(|_| ())
    }
}
/// The protocol-end of the stream; this one should impl solicit::Stream
struct ProtoStream {
    rx: mpsc::Receiver<Vec<u8>>,
    tx: mpsc::Sender<Vec<u8>>,
}

impl ProtoStream {
    fn new(rx: mpsc::Receiver<Vec<u8>>, tx: mpsc::Sender<Vec<u8>>) -> ProtoStream {
        ProtoStream {
            rx: rx,
            tx: tx,
        }
    }
}
