use std::sync::mpsc;
use std::io::Cursor;

use mio;
use mio::tcp::*;
use mio::{TryRead, TryWrite};
use mio::util::Slab;

#[derive(Debug)]
pub enum Message<M> {
    Hello,
    TryWrite,
    QueueFrame(Vec<u8>),
    Proto(M),
}

pub struct Envelope<M>(mio::Token, Message<M>);

pub enum WorkItem<P> where P: Protocol {
    AddConnection(TcpStream, Box<OnConnectionReady<P> + Send>),
    NotifyConnection(Envelope<P::Message>),
}

#[derive(Debug)]
pub struct DispatcherHandle<P> where P: Protocol {
    tx: mio::Sender<WorkItem<P>>,
}

impl<P> Clone for DispatcherHandle<P> where P: Protocol {
    fn clone(&self) -> DispatcherHandle<P> {
        DispatcherHandle {
            tx: self.tx.clone(),
        }
    }
}

impl<P> DispatcherHandle<P> where P: Protocol {
    pub fn for_loop(evtloop: &mio::EventLoop<Dispatcher<P>>) -> DispatcherHandle<P> {
        DispatcherHandle {
            tx: evtloop.channel(),
        }
    }
    pub fn add_connection(&self, stream: TcpStream, cb: Box<OnConnectionReady<P> + Send>) {
        self.notify(WorkItem::AddConnection(stream, cb));
    }
    pub fn notify_connection(&self, token: mio::Token, msg: Message<P::Message>) {
        self.notify(WorkItem::NotifyConnection(Envelope(token, msg)))
    }

    fn notify(&self, wrk: WorkItem<P>) {
        self.tx.send(wrk).unwrap();
    }
}

pub struct ConnectionHandle<P> where P: Protocol {
    handle: DispatcherHandle<P>,
    token: mio::Token,
}

impl<P> Clone for ConnectionHandle<P> where P: Protocol {
    fn clone(&self) -> ConnectionHandle<P> {
        ConnectionHandle {
            handle: self.handle.clone(),
            token: self.token,
        }
    }
}

impl<P> ConnectionHandle<P> where P: Protocol {
    pub fn new(handle: DispatcherHandle<P>, token: mio::Token) -> ConnectionHandle<P> {
        ConnectionHandle {
            handle: handle,
            token: token,
        }
    }
    pub fn notify(&self, msg: Message<P::Message>) {
        self.handle.notify_connection(self.token, msg)
    }
}

pub struct Dispatcher<P> where P: Protocol {
    connections: Slab<Connection<P>>,
}

impl<P> Dispatcher<P> where P: Protocol {
    pub fn new() -> Dispatcher<P> {
        Dispatcher {
            connections: Slab::new(1024),
        }
    }
}

pub trait OnConnectionReady<P> where P: Protocol {
    fn connection_ready(&mut self, handle: ConnectionHandle<P>) -> Result<(), ()>;
}

impl<P> Dispatcher<P> where P: Protocol {
    fn add_connection(
            &mut self,
            event_loop: &mut mio::EventLoop<Dispatcher<P>>,
            stream: TcpStream,
            mut ready: Box<OnConnectionReady<P>>) {
        let token = self.connections.insert_with(|token| {
            // Register with event loop...
            event_loop.register_opt(
                &stream,
                token,
                mio::EventSet::readable() | mio::EventSet::writable(),
                mio::PollOpt::edge()).unwrap();

            // Finally, the connection is ready...
            // TODO Either require Protocol to impl Default or figure out what params
            //      new needs.
            let conn_handle = ConnectionHandle::new(
                DispatcherHandle::for_loop(event_loop),
                token
            );
            Connection::new(token, stream, P::new(conn_handle))
        });
        let handle = token.map(|token| {
            let handle = DispatcherHandle::for_loop(event_loop);
            ConnectionHandle::new(handle, token)
        });
        // If there's no one on the other end of the channel to receive the
        // handle, the event loop shouldn't stop, so we ignore the return
        // value.
        // let _ = resolve.send(handle);
        ready.connection_ready(handle.unwrap());
    }
}

impl<P> mio::Handler for Dispatcher<P> where P: Protocol {
    type Timeout = ();
    type Message = WorkItem<P>;

    fn ready(
        &mut self,
        event_loop: &mut mio::EventLoop<Dispatcher<P>>,
        token: mio::Token,
        events: mio::EventSet
    ) {
        println!("socket is ready; token={:?}; events={:?}", token, events);
        self.connections[token].ready(event_loop, events);
    }

    fn notify(&mut self, event_loop: &mut mio::EventLoop<Dispatcher<P>>, msg: WorkItem<P>) {
        match msg {
            WorkItem::NotifyConnection(Envelope(token, msg)) => {
                self.connections[token].notify(event_loop, msg);
            },
            WorkItem::AddConnection(stream, resolve) => {
                /*
                let cb = ChannelReady {
                    resolve: resolve
                };
                */
                self.add_connection(event_loop, stream, resolve);
            },
        }
    }
}

pub struct ConnectionRef<'a, P> where P: Protocol {
    backlog: &'a mut Vec<Cursor<Vec<u8>>>,
    handle: ConnectionHandle<P>,
    writable: bool,
}

impl<'a, P> ConnectionRef<'a, P> where P: Protocol {
    pub fn queue_frame(&mut self, frame: Vec<u8>) {
        self.backlog.push(Cursor::new(frame));
    }

    pub fn write_frame(&mut self, frame: Vec<u8>) {
        self.queue_frame(frame);
        // If idle, wake it up by posting a message;
        // If blocked, do not post anything
        // If in the middle of a write (i.e. this is the result of the Connection asking the
        // protocol to provide some data), do not post anything
    }

    pub fn handle(&self) -> ConnectionHandle<P> {
        self.handle.clone()
    }
}

struct Connection<P> where P: Protocol {
    token: mio::Token,
    stream: TcpStream,
    backlog: Vec<Cursor<Vec<u8>>>,
    is_writable: bool,
    protocol: P,
    buf: Vec<u8>,
}

impl<P> Connection<P> where P: Protocol {
    fn new(token: mio::Token, stream: TcpStream, proto: P) -> Connection<P> {
        Connection {
            token: token,
            stream: stream,
            backlog: Vec::new(),
            is_writable: false,
            protocol: proto,
            buf: Vec::with_capacity(4096),
        }
    }

    fn ready(&mut self, event_loop: &mut mio::EventLoop<Dispatcher<P>>, events: mio::EventSet) {
        trace!("Connection ready: token={:?}; events={:?}", self.token, events);
        if events.is_readable() {
            self.read(event_loop);
        }
        if events.is_writable() {
            self.set_writable(true);
            // Whenever the connection becomes writable, we try a write.
            self.try_write(event_loop).unwrap();
        }
    }

    fn notify(&mut self, event_loop: &mut mio::EventLoop<Dispatcher<P>>, msg: Message<P::Message>) {
        trace!("Connection notified: token={:?}; msg={:?}", self.token, msg);
        match msg {
            Message::Hello => {
                trace!("Hi there yourself!");
            },
            Message::TryWrite => {
                trace!("Aye, aye! Will try to write something");
                self.try_write(event_loop).ok().unwrap();
            },
            Message::QueueFrame(frame) => {
                self.queue_frame(frame);
                self.try_write(event_loop).ok().unwrap();
            },
            Message::Proto(msg) => {
                let conn_ref = ConnectionRef {
                    backlog: &mut self.backlog,
                    handle: ConnectionHandle::new(
                        DispatcherHandle::for_loop(event_loop),
                        self.token),
                    writable: self.is_writable,
                };
                self.protocol.notify(msg, conn_ref);
            }
        };
    }

    fn read(&mut self, event_loop: &mut mio::EventLoop<Dispatcher<P>>) {
        // TODO Handle the case where the buffer isn't large enough to completely
        // exhaust the socket (since we're edge-triggered, we'd never get another
        // chance to read!)
        trace!("Handling read");
        match self.stream.try_read_buf(&mut self.buf) {
            Ok(Some(0)) => {
                debug!("EOF");
                // EOF
            },
            Ok(Some(n)) => {
                debug!("read {} bytes", n);
                let drain = {
                    let conn_ref = ConnectionRef {
                        backlog: &mut self.backlog,
                        handle: ConnectionHandle::new(
                            DispatcherHandle::for_loop(event_loop),
                            self.token),
                        writable: self.is_writable,
                    };
                    let buf = &self.buf;
                    self.protocol.on_data(buf, conn_ref).unwrap()
                };

                // TODO Would a circular buffer be better than draining here...?
                // Though it might not be ... there shouldn't be that many elems
                // to copy to the front usually... this would give an advantage
                // to later reads as there will never be a situation where a mini
                // read needs to be done because we're about to wrap around in the
                // buffer ... on the other hand, a mini read every-so-often might
                // even be okay? If it eliminates copies...
                trace!("Draining... {}/{}", drain, self.buf.len());
                {
                    let _ = self.buf.drain(..drain);
                }
                trace!("Done.");
            },
            Ok(None) => {
                debug!("read WOULDBLOCK");
            },
            Err(e) => {
                panic!("got an error trying to read; err={:?}", e);
            },
        };
    }

    fn try_write(&mut self, event_loop: &mut mio::EventLoop<Dispatcher<P>>) -> Result<(), ()> {
        trace!("-> Attempting to write!");
        if !self.is_writable {
            trace!("Currently not writable!");
            return Ok(())
        }
        // Anything that's already been queued is considered the first priority to
        // push out to the peer. We write as much of this as possible.
        try!(self.write_backlog());
        if self.is_writable {
            trace!("Backlog flushed; notifying protocol");
            // If we're still writable, tell the protocol so that it can react to that and
            // potentially provide more.
            {
                let conn_ref = ConnectionRef {
                    backlog: &mut self.backlog,
                    handle: ConnectionHandle::new(
                        DispatcherHandle::for_loop(event_loop),
                        self.token),
                    writable: self.is_writable,
                };
                self.protocol.ready_write(conn_ref);
            }
            // Flush whatever the protocol might have added...
            // TODO Should we allow the protocol to queue more than once (this could hold
            // up the event loop if the protocol keeps adding stuff...)
            try!(self.write_backlog());
        }

        Ok(())
    }

    fn write_backlog(&mut self) -> Result<(), ()> {
        loop {
            if self.backlog.is_empty() {
                trace!("Backlog already empty.");
                return Ok(());
            }
            trace!("Trying a write from the backlog; items in backlog - {}", self.backlog.len());
            let status = {
                let buf = &mut self.backlog[0];
                match self.stream.try_write_buf(buf) {
                    Ok(Some(_)) if buf.get_ref().len() == buf.position() as usize => {
                        trace!("Full frame written!");
                        trace!("{:?}", buf);
                        WriteStatus::Full
                    },
                    Ok(Some(sz)) => {
                        trace!("Partial write: {} bytes", sz);
                        WriteStatus::Partial
                    },
                    Ok(None) => {
                        trace!("Write WOULDBLOCK");
                        WriteStatus::WouldBlock
                    },
                    Err(e) => {
                        panic!("Error writing! {:?}", e);
                    },
                }
            };
            match status {
                WriteStatus::Full => {
                    // TODO A deque or even maybe a linked-list would be better than a vec
                    // for this type of thing...
                    self.backlog.remove(0);
                },
                WriteStatus::Partial => {},
                WriteStatus::WouldBlock => {
                    self.set_writable(false);
                    break;
                },
            };
        }
        Ok(())
    }

    fn set_writable(&mut self, w: bool) { self.is_writable = w; }

    pub fn queue_frame(&mut self, frame: Vec<u8>) {
        self.backlog.push(Cursor::new(frame));
    }
}

enum WriteStatus {
    Full,
    Partial,
    WouldBlock,
}

pub trait Protocol: Sized + 'static {
    type Message: Send + ::std::fmt::Debug;

    fn new(conn: ConnectionHandle<Self>) -> Self;
    fn on_data(&mut self, buf: &[u8], conn: ConnectionRef<Self>) -> Result<usize, ()>;
    fn ready_write(&mut self, conn: ConnectionRef<Self>);
    fn notify(&mut self, msg: Self::Message, conn: ConnectionRef<Self>);
}

pub struct Dummy;
#[derive(Debug)]
pub struct DummyMsg;
impl Protocol for Dummy {
    type Message = DummyMsg;

    fn new(_: ConnectionHandle<Self>) -> Dummy {
        Dummy
    }

    fn on_data(&mut self, buf: &[u8], _conn: ConnectionRef<Dummy>) -> Result<usize, ()> {
        trace!("Dummy: Got some data!");
        Ok(buf.len())
    }

    fn ready_write(&mut self, mut conn: ConnectionRef<Dummy>) {
        trace!("Hello, from dummy");
        conn.queue_frame((&b"Hello from Dummy!"[..]).into());
    }

    fn notify(&mut self, msg: DummyMsg, _conn: ConnectionRef<Dummy>) {
        trace!("Dummy notified: msg={:?}", msg);
    }
}
