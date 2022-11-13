#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver as Receiver, UnboundedSender as Sender},
    lock::Mutex as AsyncMutex,
    StreamExt,
};
use std::{
    collections::hash_map::HashMap,
    mem,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
};

type Id = u32;
type AtomicId = AtomicU32;

struct Tx<T>(Id, T);
struct Rx<R>(Id, Option<R>);

pub struct Requester<T, R> {
    sender: Sender<Tx<T>>,
    receiver: AsyncMutex<Receiver<Rx<R>>>,
    /// Buffer contains ids of all all Responses waiting for response.
    /// Possible values and their meaning:
    /// + `None` - response may arrive in future.
    /// + `Some(None)` - response will never arrive.
    /// + `Some(Some(message))` - response arrived but hasn't been extracted by Request.
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

pub struct Request<'a, R> {
    id: Id,
    receiver: &'a AsyncMutex<Receiver<Rx<R>>>,
    buffer: &'a Mutex<HashMap<Id, Option<Option<R>>>>,
}

impl<T, R> Requester<T, R> {
    pub fn request(&self, message: T) -> Result<Request<'_, R>, T> {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let mut buffer = self.buffer.lock().unwrap();
        debug_assert!(!buffer.contains_key(&id));
        match self.sender.unbounded_send(Tx(id, message)) {
            Ok(()) => assert!(buffer.insert(id, None).is_none()),
            Err(err) => return Err(err.into_inner().1),
        }
        Ok(Request {
            id,
            receiver: &self.receiver,
            buffer: &self.buffer,
        })
    }
}

impl<'a, R> Request<'a, R> {
    fn try_take_from_buffer(&self) -> Option<Option<R>> {
        self.buffer
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .unwrap()
            .take()
    }

    pub async fn get_response(self) -> Option<R> {
        if let Some(value) = self.try_take_from_buffer() {
            return value;
        }

        let mut guard = self.receiver.lock().await;

        // Check the buffer once more to detect insertion right before guard but after previous check.
        if let Some(value) = self.try_take_from_buffer() {
            return value;
        }

        while let Some(Rx(id, message)) = guard.next().await {
            if id == self.id {
                return message;
            }
            if let Some(value) = self.buffer.lock().unwrap().get_mut(&id) {
                assert!(value.replace(message).is_none());
            }
        }

        None
    }
}

impl<'a, R> Drop for Request<'a, R> {
    fn drop(&mut self) {
        self.buffer.lock().unwrap().remove(&self.id).unwrap();
    }
}

pub struct Response<'a, R> {
    id: Id,
    sender: &'a mut Sender<Rx<R>>,
}

impl<T, R> Responder<T, R> {
    pub async fn next(&mut self) -> Option<(T, Response<'_, R>)> {
        let Tx(id, message) = self.receiver.next().await?;
        Some((
            message,
            Response {
                id,
                sender: &mut self.sender,
            },
        ))
    }
}

impl<'a, R> Response<'a, R> {
    pub fn respond(self, message: R) {
        let _ = self.sender.unbounded_send(Rx(self.id, Some(message)));
        // Suppress calling `drop`.
        mem::forget(self);
    }
}

impl<'a, R> Drop for Response<'a, R> {
    fn drop(&mut self) {
        let _ = self.sender.unbounded_send(Rx(self.id, None));
    }
}
