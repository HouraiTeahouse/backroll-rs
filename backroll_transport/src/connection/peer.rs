use async_channel::{TryRecvError, TrySendError};
use core::hash::Hash;
use std::{collections::HashMap, ops::Deref};

#[derive(Clone)]
pub struct BidirectionalAsyncChannel<T> {
    incoming: async_channel::Receiver<T>,
    outgoing: async_channel::Sender<T>,
}

impl<T> BidirectionalAsyncChannel<T> {
    /// Creates a pair of connected Peers without limitations on
    /// how many messages can be buffered.
    pub fn create_unbounded_pair() -> (Self, Self) {
        Self::create_pair(async_channel::unbounded(), async_channel::unbounded())
    }

    /// Creates a pair of connected Peers with a limited capacity
    /// for many messages can be buffered in either direction.
    pub fn create_bounded_pair(capacity: usize) -> (Self, Self) {
        Self::create_pair(
            async_channel::bounded(capacity),
            async_channel::bounded(capacity),
        )
    }

    /// Sends a message to the connected peer.
    ///
    /// If the send buffer is full, this method waits until there is
    /// space for a message.
    ///
    /// If the peer is disconnected, this method returns an error.
    #[inline]
    pub fn send(&self, message: T) -> async_channel::Send<'_, T> {
        self.outgoing.send(message)
    }

    /// Receives a message from the connected peer.
    ///
    /// If there is no pending messages, this method waits until there is a
    /// message.
    ///
    /// If the peer is disconnected, this method receives a message or returns
    /// an error if there are no more messages.
    #[inline]
    pub fn recv(&self) -> async_channel::Recv<'_, T> {
        self.incoming.recv()
    }

    /// Attempts to send a message to the connected peer.
    #[inline]
    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        self.outgoing.try_send(message)
    }

    /// Attempts to receive a message from the connected peer.
    #[inline]
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.incoming.try_recv()
    }

    /// Returns true if the associated peer is still connected.
    pub fn is_connected(&self) -> bool {
        !self.incoming.is_closed() && !self.outgoing.is_closed()
    }

    /// Disconnects the paired Peers from either end. Any future attempts
    /// to send messages in either direction will fail, but any messages
    /// not yet recieved.
    ///
    /// If the Peer, or it's constituent channels were cloned, all of the
    /// cloned instances will appear disconnected.
    pub fn disconnect(&self) {
        self.outgoing.close();
        self.incoming.close();
    }

    /// Gets the raw sender for the peer.
    pub fn sender(&self) -> async_channel::Sender<T> {
        self.outgoing.clone()
    }

    /// Gets the raw reciever for the peer.
    pub fn reciever(&self) -> async_channel::Receiver<T> {
        self.incoming.clone()
    }

    /// The number of messages that are currently buffered in the
    /// send queue. Returns 0 if the Peer is disconnected.
    pub fn pending_send_count(&self) -> usize {
        self.outgoing.len()
    }

    /// The number of messages that are currently buffered in the
    /// recieve queue. Returns 0 if the Peer is disconnected.
    pub fn pending_recv_count(&self) -> usize {
        self.incoming.len()
    }

    fn create_pair(
        a: (async_channel::Sender<T>, async_channel::Receiver<T>),
        b: (async_channel::Sender<T>, async_channel::Receiver<T>),
    ) -> (Self, Self) {
        let (a_send, a_recv) = a;
        let (b_send, b_recv) = b;
        let a = Self {
            incoming: a_recv,
            outgoing: b_send,
        };
        let b = Self {
            incoming: b_recv,
            outgoing: a_send,
        };
        (a, b)
    }
}

/// A bidirectional channel for binary messages.
#[derive(Clone)]
pub struct Peer(BidirectionalAsyncChannel<Box<[u8]>>);

impl Peer {
    /// Creates a pair of connected Peers without limitations on
    /// how many messages can be buffered.
    pub fn create_unbounded_pair() -> (Self, Self) {
        let (a, b) = BidirectionalAsyncChannel::create_unbounded_pair();
        (Self(a), Self(b))
    }

    /// Creates a pair of connected Peers with a limited capacity
    /// for many messages can be buffered in either direction.
    pub fn create_bounded_pair(capacity: usize) -> (Self, Self) {
        let (a, b) = BidirectionalAsyncChannel::create_bounded_pair(capacity);
        (Self(a), Self(b))
    }
}

impl Deref for Peer {
    type Target = BidirectionalAsyncChannel<Box<[u8]>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Default)]
pub struct Peers<T>(HashMap<T, Peer>)
where
    T: Eq + Hash;

impl<T: Eq + Hash> Peers<T> {
    /// Gets a Peer by it's ID, if available.
    pub fn get(&self, id: &T) -> Option<Peer> {
        self.0.get(&id).cloned()
    }

    /// Checks if the store has a connection to the given ID.
    pub fn contains(&self, id: &T) -> bool {
        self.0.contains_key(&id)
    }

    /// Creates an unbounded peer pair and stores one end, mapping it to the
    /// provided ID, returning the other end.
    ///
    /// If a peer was previous stored at the given ID, it will be dropped and
    /// replaced.
    #[must_use]
    pub fn create_unbounded(&mut self, id: T) -> Peer {
        let (a, b) = Peer::create_unbounded_pair();
        self.0.insert(id, a);
        b
    }

    /// Removes a connection by it's ID.
    ///
    /// A no-op if there no Peer with the given ID.
    pub fn disconnect(&mut self, id: T) {
        self.0.remove(&id);
    }

    /// Removes all peers that are disconnected.
    pub fn flush_disconnected(&mut self) {
        self.0.retain(|_, v| v.is_connected())
    }
}
