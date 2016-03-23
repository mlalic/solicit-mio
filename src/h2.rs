use std::sync::mpsc;
use std::io::{Cursor, Read};

use solicit::http::frame::{Frame, RawFrame, FrameIR};
use solicit::http::{HttpScheme, HttpResult, Header, StreamId};
use solicit::http::connection::{HttpConnection, SendFrame, ReceiveFrame, HttpFrame};
use solicit::http::client::{write_preface, ClientConnection, RequestStream};
use solicit::http::session::{DefaultSessionState, SessionState, DefaultStream};
use solicit::http::session::{Stream as HttpStream, StreamDataChunk, StreamDataError, StreamState, Client as ClientMarker, self};

use evtloop::{Protocol, ConnectionRef, ConnectionHandle, Message};

struct FakeSend;
impl SendFrame for FakeSend {
    #[inline]
    fn send_frame<F: FrameIR>(&mut self, frame: F) -> HttpResult<()> {
        trace!("FakeSend::send_frame");
        Ok(())
    }
}

// TODO Replace with a send-the-short-way once `solicit` allows us to!
//      It's the long way because even though the HttpConnection at the point when it uses the
//      SendFrame methods is executing within the event loop (and thus has exclusive ownership of
//      everything), we still send a QueueFrame message to the loop instead of directly invoking
//      the equivalent functionality on the ConnectionRef.
//      Solicit needs to facilitate this by changing the HttpConnection API to not require an
//      owned SendFrame instance, but only one that it gets from the session layer in its
//      handle/send methods (similar to how the session delegate/callbacks are passed).
struct SendLongWay {
    handle: ConnectionHandle<Http2>,
}
impl SendFrame for SendLongWay {
    #[inline]
    fn send_frame<F: FrameIR>(&mut self, frame: F) -> HttpResult<()> {
        trace!("SendLongWay::send_raw_frame");
        let mut buf = Cursor::new(Vec::with_capacity(1024));
        try!(frame.serialize_into(&mut buf));
        self.handle.notify(Message::QueueFrame(buf.into_inner()));
        Ok(())
    }
}

struct SendDirect<'brw, 'conn> where 'conn: 'brw {
    conn: &'brw mut ConnectionRef<'conn, Http2>,
}
impl<'a, 'b> SendFrame for SendDirect<'a, 'b> {
    fn send_frame<F: FrameIR>(&mut self, frame: F) -> HttpResult<()> {
        let mut buf = Cursor::new(Vec::with_capacity(1024));
        try!(frame.serialize_into(&mut buf));
        self.conn.queue_frame(buf.into_inner());
        Ok(())
    }
}

struct FakeReceive;
impl ReceiveFrame for FakeReceive {
    fn recv_frame(&mut self) -> HttpResult<HttpFrame> {
        panic!("Should never have been called!");
    }
}

struct WrappedReceive<'a> {
    frame: Option<RawFrame<'a>>,
}

impl<'a> WrappedReceive<'a> {
    fn parse(buf: &'a [u8]) -> Option<WrappedReceive<'a>> {
        RawFrame::parse(buf).map(|frame| WrappedReceive {
            frame: Some(frame),
        })
    }
    pub fn frame(&self) -> Option<&RawFrame<'a>> { self.frame.as_ref() }
}
impl<'a> ReceiveFrame for WrappedReceive<'a> {
    fn recv_frame(&mut self) -> HttpResult<HttpFrame> {
        // TODO HttpFrame should also allow for borrowed frame buffers. As it stands, at this point
        //      we have to make a copy!!! This is not the fault of the ReceiveFrame abstraction,
        //      though. The same would happen if the HttpConn itself parsed a raw buffer into a
        //      frame, since it'd need to create an HttpFrame at some point in order to
        //      correspondingly handle it...
        HttpFrame::from_raw(self.frame.as_ref().unwrap())
    }
}

struct BoxStream {
    inner: Box<RequestDelegate>,
}
impl HttpStream for BoxStream {
    fn new_data_chunk(&mut self, data: &[u8]) { self.inner.new_data_chunk(data) }
    fn set_headers(&mut self, headers: Vec<Header>) { self.inner.set_headers(headers) }
    fn set_state(&mut self, state: StreamState) { self.inner.set_state(state) }

    fn get_data_chunk(&mut self, buf: &mut [u8]) -> Result<StreamDataChunk, StreamDataError> {
        self.inner.get_data_chunk(buf)
    }

    /// Returns the current state of the stream.
    fn state(&self) -> StreamState { self.inner.state() }
}
pub struct Http2 {
    streams: Vec<ProtoStream>,
    init: bool,
    conn: ClientConnection<DefaultSessionState<ClientMarker, BoxStream>>,
    next_stream_id: StreamId,
}

impl Http2 {
    fn new_stream<'n, 'v>(
            &mut self,
            method: &'v [u8],
            path: &'v [u8],
            extras: &[Header<'n, 'v>],
            body: Option<Vec<u8>>,
            stream: Box<RequestDelegate>)
            -> RequestStream<'n, 'v, BoxStream> {
        // TODO Set ID
        // TODO Figure out when to locally close streams that have no data in order to send just
        //      the headers with a request...

        let mut headers: Vec<Header> = vec![
            Header::new(b":method", method),
            Header::new(b":path", path),
            Header::new(b":authority", &b"http2bin.org"[..]),
            Header::new(b":scheme", self.conn.scheme().as_bytes().to_vec()),
        ];
        headers.extend(extras.iter().map(|h| h.clone()));

        RequestStream {
            headers: headers,
            stream: BoxStream { inner: stream },
        }
    }
}

impl Protocol for Http2 {
    type Message = HttpMsg;

    fn new(conn: ConnectionHandle<Http2>) -> Http2 {
        let raw_conn = HttpConnection::new(HttpScheme::Http);
        let state = session::default_client_state();
        Http2 {
            streams: Vec::new(),
            init: false,
            conn: ClientConnection::with_connection(raw_conn, state),
            next_stream_id: 1,
        }
    }

    fn on_data<'a>(&mut self, buf: &[u8], mut conn: ConnectionRef<Http2>) -> Result<usize, ()> {
        trace!("Http2: Received something back");
        let mut total_consumed = 0;
        loop {
            match WrappedReceive::parse(&buf[total_consumed..]) {
                None => {
                    // No frame available yet. We consume nothing extra and wait for more data to
                    // become available to retry.
                    let done = self.conn.state.get_closed();
                    for stream in done {
                        info!("Got response!");
                    }

                    return Ok(total_consumed);
                },
                Some(mut receiver) => {
                    let len = receiver.frame().unwrap().len();
                    debug!("Handling an HTTP/2 frame of total size {}", len);
                    let mut sender = SendDirect { conn: &mut conn };
                    try!(self.conn.handle_next_frame(&mut receiver, &mut sender).map_err(|_| ()));
                    total_consumed += len;
                },
            }
        }
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
            // Write preface
            let mut buf = Vec::new();
            write_preface(&mut buf).unwrap();
            conn.queue_frame(buf);
            self.init = true;
        } else {
            let mut sender = SendDirect { conn: &mut conn };
            self.conn.send_next_data(&mut sender);
        }
    }

    fn notify(&mut self, msg: HttpMsg, mut conn: ConnectionRef<Http2>) {
        trace!("Http2 notified: msg={:?}", msg);
        if !self.init {
            // Write preface
            let mut buf = Vec::new();
            write_preface(&mut buf).unwrap();
            conn.queue_frame(buf);
            self.init = true;
        }
        match msg {
            HttpMsg::NewStream(mut request) => {
                debug!("Also sending a request");
                let stream = self.new_stream(
                    b"POST",
                    b"/post",
                    &[],
                    Some(b"Hello, World!"[..].into()),
                    request);
                let mut sender = SendDirect { conn: &mut conn };
                let id = self.conn.start_request(stream, &mut sender).unwrap();
                // TODO Ewww!
                self.conn.state.get_stream_mut(id).unwrap().inner.started(id);
            },
        }
    }
}

pub trait RequestDelegate: HttpStream {
    fn started(&mut self, stream_id: StreamId);
}
pub enum HttpMsg {
    NewStream(Box<RequestDelegate + Send>),
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

/*
impl HttpStream for ProtoStream {
    fn new(stream_id: StreamId) -> Self {

    }
    fn new_data_chunk(&mut self, data: &[u8]);
    fn set_headers(&mut self, headers: Vec<Header>);
    fn set_state(&mut self, state: StreamState);

    fn get_data_chunk(&mut self, buf: &mut [u8]) -> Result<StreamDataChunk, StreamDataError>;

    /// Returns the ID of the stream.
    fn id(&self) -> StreamId;
    /// Returns the current state of the stream.
    fn state(&self) -> StreamState;
}
*/
