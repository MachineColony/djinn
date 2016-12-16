use std::{net, thread};
use std::io::Error;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tokio_core::io::Io;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;
use tokio_proto::TcpClient;
use tokio_service::Service;
use futures::Future;
use futures::sink::Sink;
use futures::stream::Stream;
use super::actor::{Actor, ActorRef, ActorVecRef};
use super::path::ActorPath;
use super::message::Message;
use super::protocol::{MsgPackProtocol, MsgPackCodec};

#[derive(RustcDecodable, RustcEncodable, Debug)]
pub enum RoutingMessage<T: Message> {
    Ok,
    Message {
        sender: ActorPath,
        recipient: usize,
        message: T,
    },
}

pub struct Router<A: Actor + 'static> {
    actors: Arc<Mutex<HashMap<usize, ActorRef<A>>>>,
}

impl<A> Router<A>
    where A: Actor + 'static
{
    pub fn new(actors: ActorVecRef<A>) -> Router<A> {
        let mut lookup = HashMap::<usize, ActorRef<A>>::new();
        {
            let actors = actors.clone();
            let actors = actors.read().unwrap();
            for actor in actors.iter() {
                let actor_r = actor.read().unwrap();
                lookup.insert(actor_r.id(), actor.clone());
            }
        }
        Router { actors: Arc::new(Mutex::new(lookup)) }
    }

    pub fn serve(&self, addr: String) {
        let actors = self.actors.clone();
        thread::spawn(move || {
            let mut core = Core::new().unwrap();
            let handle = core.handle();
            let addr = addr.parse().unwrap();
            let tcp_socket = TcpListener::bind(&addr, &handle).unwrap();
            println!("Listening on: {}", addr);

            let done = tcp_socket.incoming()
                .for_each(move |(socket, addr)| {
                    println!("Received connection from: {}", addr);

                    let actors = actors.clone();
                    let (sink, stream) =
                            socket.framed(MsgPackCodec::<RoutingMessage<A::M>,
                                                       RoutingMessage<A::M>>::new())
                                .split();
                    let conn = stream.forward(sink.with(move |req| {
                            let req: RoutingMessage<A::M> = req;
                            println!("{:?}", req);
                            let res: Result<RoutingMessage<A::M>, Error> = match req {
                                RoutingMessage::Message { sender, recipient, message } => {
                                    match actors.lock().unwrap().get(&recipient) {
                                        Some(actor) => {
                                            let actor_r = actor.read().unwrap();
                                            let mut inbox = actor_r.inbox().write().unwrap();
                                            inbox.push(message);
                                            Ok(RoutingMessage::Ok)
                                        }
                                        None => Ok(RoutingMessage::Ok),
                                    }
                                }
                                RoutingMessage::Ok => Ok(RoutingMessage::Ok),
                            };
                            res
                        }))
                        .then(|_| Ok(()));
                    handle.spawn(conn);
                    Ok(())
                });
            let _ = core.run(done);
        });
    }

    pub fn send_msg(&mut self,
                    message: A::M,
                    sender: ActorPath,
                    recipient: ActorPath)
                    -> Result<RoutingMessage<A::M>, String> {
        match recipient {
            ActorPath::Local { id } => {
                match self.actors.lock().unwrap().get(&id) {
                    Some(actor) => {
                        let actor_r = actor.read().unwrap();
                        let mut inbox = actor_r.inbox().write().unwrap();
                        inbox.push(message);
                    }
                    None => (),
                }
                Ok(RoutingMessage::Ok)
            }
            ActorPath::Remote { addr, id } => {
                let msg = RoutingMessage::Message {
                    sender: sender,
                    recipient: id,
                    message: message,
                };
                let mut core = Core::new().unwrap();
                let handle = core.handle();
                let addr = addr.0;
                println!("connecting to {}", addr);
                let proto: MsgPackProtocol<RoutingMessage<A::M>, RoutingMessage<A::M>> =
                    MsgPackProtocol::new();
                let client = TcpClient::new(proto).connect(&addr, &handle);
                core.run(client.then(|result| {
                        match result {
                            Ok(c) => Ok(c.call(msg)),
                            Err(e) => Err(e),
                        }
                    })
                    .then(|_| Ok(RoutingMessage::Ok))) // TODO should return response
            }
        }
    }
}
