use std::marker::PhantomData;

// I'm making it unreasonably complex already...

trait Message<I> {
    type Streaming;
}

trait MessageOut {
    type Streaming;
    fn headers(&mut self, headers: Vec<Header>);
    fn start(self) -> Self::Streaming;
}

trait MessageIn {
    type Streaming;
}
