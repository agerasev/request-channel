#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver as Receiver, UnboundedSender as Sender},
    lock::Mutex as AsyncMutex,
    ready, FutureExt, StreamExt,
};
use std::{
    collections::hash_map::HashMap,
    future::Future,
    mem,
    pin::Pin,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
    task::{Context, Poll},
};

type Id = u32;
type AtomicId = AtomicU32;

struct Tx<T>(Id, T);
struct Rx<R>(Id, Option<R>);

pub struct Requester<T, R> {
    sender: Sender<Tx<T>>,
    receiver: AsyncMutex<Receiver<Rx<R>>>,
    /// Buffer contains ids of all all handles waiting for response.
    /// Possible values and their meaning:
    /// + `None` - response may arrive in future.
    /// + `Some(None)` - response will never arrive.
    /// + `Some(Some(message))` - response arrived but hasn't been extracted by handle.
    buffer: Mutex<HashMap<Id, Option<Option<R>>>>,
    counter: AtomicId,
}

pub struct Responder<T, R> {
    receiver: Receiver<Tx<T>>,
    sender: Sender<Rx<R>>,
}

pub fn channel<T, R>() -> (Requester<T, R>, Responder<T, R>) {
    let (tx_sender, tx_receiver) = unbounded::<Tx<T>>();
    let (rx_sender, rx_receiver) = unbounded::<Rx<R>>();
    (
        Requester {
            sender: tx_sender,
            receiver: AsyncMutex::new(rx_receiver),
            buffer: Mutex::new(HashMap::new()),
            counter: AtomicId::new(0),
        },
        Responder {
            receiver: tx_receiver,
            sender: rx_sender,
        },
    )
}

pub struct Response<'a, R> {
    id: Id,
    receiver: &'a AsyncMutex<Receiver<Rx<R>>>,
    buffer: &'a Mutex<HashMap<Id, Option<Option<R>>>>,
}

impl<T, R> Requester<T, R> {
    pub fn request(&self, message: T) -> Result<Response<'_, R>, T> {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let mut buffer = self.buffer.lock().unwrap();
        debug_assert!(!buffer.contains_key(&id));
        match self.sender.unbounded_send(Tx(id, message)) {
            Ok(()) => assert!(buffer.insert(id, None).is_none()),
            Err(err) => return Err(err.into_inner().1),
        }
        Ok(Response {
            id,
            receiver: &self.receiver,
            buffer: &self.buffer,
        })
    }
}

impl<'a, R> Response<'a, R> {
    fn try_take_from_buffer(&self) -> Option<Option<R>> {
        self.buffer
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .unwrap()
            .take()
    }
}

impl<'a, R> Future for Response<'a, R> {
    type Output = Option<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(value) = self.try_take_from_buffer() {
            return Poll::Ready(value);
        }

        let mut guard = ready!(self.receiver.lock().poll_unpin(cx));

        // Check the buffer once more to detect insertion right before guard but after previous check.
        if let Some(value) = self.try_take_from_buffer() {
            return Poll::Ready(value);
        }

        while let Some(Rx(id, message)) = ready!(guard.poll_next_unpin(cx)) {
            if id == self.id {
                return Poll::Ready(message);
            }
            if let Some(value) = self.buffer.lock().unwrap().get_mut(&id) {
                assert!(value.replace(message).is_none());
            }
        }

        Poll::Ready(None)
    }
}

impl<'a, R> Unpin for Response<'a, R> {}

impl<'a, R> Drop for Response<'a, R> {
    fn drop(&mut self) {
        self.buffer.lock().unwrap().remove(&self.id).unwrap();
    }
}

pub struct Request<'a, R> {
    id: Id,
    sender: &'a mut Sender<Rx<R>>,
}

impl<T, R> Responder<T, R> {
    pub async fn next(&mut self) -> Option<(T, Request<'_, R>)> {
        let Tx(id, message) = self.receiver.next().await?;
        Some((
            message,
            Request {
                id,
                sender: &mut self.sender,
            },
        ))
    }
}

impl<'a, R> Request<'a, R> {
    pub fn response(self, message: R) {
        let _ = self.sender.unbounded_send(Rx(self.id, Some(message)));
        // Suppress calling `drop`.
        mem::forget(self);
    }
}

impl<'a, R> Drop for Request<'a, R> {
    fn drop(&mut self) {
        let _ = self.sender.unbounded_send(Rx(self.id, None));
    }
}
