#[macro_use] extern crate log;
extern crate solicit_mio;
extern crate solicit;
extern crate zukunft;
extern crate env_logger;
extern crate mio;

use std::thread;
use std::sync::mpsc;
use std::io::{Write, Read, Cursor, self};

use mio::tcp::TcpStream;

use solicit_mio::evtloop::{DispatcherHandle, Message, OnConnectionReady, Protocol, ConnectionHandle};
use solicit_mio::h2::{HttpMsg, RequestDelegate, Http2};

use solicit_mio::evtloop::Dummy;
use solicit_mio::request::{FreshRequest, Request};

use solicit_mio::simple::{Simple, Msg};

use zukunft::Future;
use solicit::http::{StreamId, Header};
use solicit::http::session::{Stream, StreamState, StreamDataChunk, StreamDataError};

struct EchoDelegate {
    state: StreamState,
}
impl RequestDelegate for EchoDelegate {
    fn started(&mut self, stream_id: StreamId) {
        info!("Started stream: id={:?}", stream_id);
    }
}
impl Stream for EchoDelegate {
    fn new_data_chunk(&mut self, data: &[u8]) {
        info!("Got data chunk: data={:?}", data);
    }
    fn set_headers(&mut self, headers: Vec<Header>) {
        info!("Got headers");
    }
    fn set_state(&mut self, state: StreamState) { self.state = state; }

    fn get_data_chunk(&mut self, buf: &mut [u8]) -> Result<StreamDataChunk, StreamDataError> {
        buf[0] = b'A';
        buf[1] = b'B';
        self.close_local();
        info!("Clsoed local stream -- {:?}", self.state());
        Ok(StreamDataChunk::Last(2))
    }

    /// Returns the current state of the stream.
    fn state(&self) -> StreamState { self.state }
}

pub struct StreamResponse {
    stream: mpsc::Receiver<Vec<u8>>,
    leftover: Option<Cursor<Vec<u8>>>,
}

impl StreamResponse {
    fn get_next_chunk(&mut self) -> Cursor<Vec<u8>> {
        match self.stream.recv() {
            Ok(buf) => Cursor::new(buf),
            Err(_) => panic!("Foo"),
        }
    }
}

impl io::Read for StreamResponse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut cursor = match self.leftover.take() {
            Some(cursor) => cursor,
            None => self.get_next_chunk(),
        };
        let res = cursor.read(buf);
        if cursor.position() as usize == cursor.get_ref().len() {
            self.leftover = Some(cursor);
        }

        res
    }
}
struct ChannelDelegate {
    state: StreamState,
    resolve: mpsc::Sender<StreamResponse>,
    response_chunk_tx: Option<mpsc::Sender<Vec<u8>>>,
}
impl RequestDelegate for ChannelDelegate {
    // This is intended to be different from the `on_id_assigned` that is to be added to the Stream
    // trait shortly in that we should provide some sort of protocol/request handle to the delegate
    // so that it can post messages to the event loop (e.g. to wake up the loop once some async
    // operation causes it to get more data!)
    fn started(&mut self, stream_id: StreamId) {
        info!("Started stream: id={:?}", stream_id);
    }
}
impl Stream for ChannelDelegate {
    fn new_data_chunk(&mut self, data: &[u8]) {
        info!("Got data chunk: data={:?}", data);
        self.response_chunk_tx.as_mut().map(|tx| tx.send(data.into()));
    }
    fn set_headers(&mut self, headers: Vec<Header>) {
        info!("Got headers; starting response");
        let (tx, rx) = mpsc::channel();
        self.response_chunk_tx = Some(tx);
        self.resolve.send(StreamResponse {
            stream: rx,
            leftover: None,
        });
    }
    fn set_state(&mut self, state: StreamState) { self.state = state; }

    fn get_data_chunk(&mut self, buf: &mut [u8]) -> Result<StreamDataChunk, StreamDataError> {
	    // TODO Obtain this from some other buffer/`Read`er...
        buf[0] = b'A';
        buf[1] = b'B';
        self.close_local();
        Ok(StreamDataChunk::Last(2))
    }

    /// Returns the current state of the stream.
    fn state(&self) -> StreamState { self.state }
}

pub struct ChannelReady<P> where P: Protocol {
    resolve: mpsc::Sender<Option<ConnectionHandle<P>>>,
}

impl<P> OnConnectionReady<P> for ChannelReady<P> where P: Protocol {
    fn connection_ready(&mut self, handle: ConnectionHandle<P>) -> Result<(), ()> {
        self.resolve.send(Some(handle)).map_err(|_| ())
    }
}


fn main() {
    env_logger::init().unwrap();
    // simple();
    // dummy();
    // solicit_mio::start();
    let mut event_loop = mio::EventLoop::new().unwrap();
    let handle = DispatcherHandle::for_loop(&event_loop);
    thread::spawn(move || {
        let mut dispatcher = solicit_mio::evtloop::Dispatcher::<Http2>::new();
        event_loop.run(&mut dispatcher).unwrap();
    });

    // let conn = TcpStream::connect(&"127.0.0.1:8080".parse().unwrap()).unwrap();
    let conn = TcpStream::connect(&"104.131.161.90:80".parse().unwrap()).unwrap();
    let (tx, rx) = mpsc::channel();
    let cb = Box::new(ChannelReady { resolve: tx });
    handle.add_connection(conn, cb);
    let future = zukunft::ChannelFuture::from_receiver(rx);

    let future = future.bind(|conn_handle| {
        let conn_handle = conn_handle.unwrap();
        // let cb = EchoDelegate { state: StreamState::Open };
        let (tx, rx) = mpsc::channel();
        let cb = ChannelDelegate {
            state: StreamState::Open,
            resolve: tx,
            response_chunk_tx: None,
        };
        conn_handle.notify(Message::Proto(HttpMsg::NewStream(Box::new(cb))));
        zukunft::ChannelFuture::from_receiver(rx).then(|mut stream| {
            println!("Response starting!");
            let mut buf = vec![0; 5];
            stream.read(&mut buf);
            println!("Read --> {:?}", buf);
        })
    });

    /*
    let future = future.then(|mut request| {
        // request.write(&b"Hello world!"[..]);
        request.send()
    });
    */

    let mut resp = future.await();
    // let conn_handle = future_handle.recv().unwrap().unwrap();
    // println!("Received my handle woot");
    // conn_handle.notify(Message::Hello);


    // let req = FreshRequest::new(conn_handle);
    // let mut req = req.start().await().unwrap();
    // req.write(&b"Hello world!"[..]);
    // let mut resp = req.send();
    // let read = resp.read(&mut buf);
    // info!("Read: total={:?}; buf={:?}", read, buf);
    thread::sleep_ms(100000);
}
