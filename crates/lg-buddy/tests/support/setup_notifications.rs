use dbus::{
    arg::PropMap,
    blocking::Connection,
    channel::{MatchingReceiver, Sender},
    Message,
};
use dbus_crossroads::Crossroads;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const INTERFACE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

pub struct Delivery {
    pub sender: String,
    pub id: u32,
    pub summary: String,
    pub body: String,
    pub actions: Vec<String>,
}

pub struct NotificationAgent {
    pub deliveries: mpsc::Receiver<Delivery>,
    pub closes: mpsc::Receiver<u32>,
    release: mpsc::Sender<()>,
    actions: mpsc::Sender<(String, u32, String, Option<String>)>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl NotificationAgent {
    pub fn new(address: &str) -> Self {
        let address = address.to_string();
        let (delivered, deliveries) = mpsc::channel();
        let (closed, closes) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (actions, invoked) = mpsc::channel::<(String, u32, String, Option<String>)>();
        let (ready, started) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let handle = thread::spawn(move || {
            let connection = Connection::new_address(&address).unwrap();
            connection
                .request_name(INTERFACE, true, true, true)
                .unwrap();
            let mut crossroads = Crossroads::new();
            let interface = crossroads.register(INTERFACE, move |builder| {
                builder.method("GetCapabilities", (), ("capabilities",), |_, _, ()| {
                    Ok((vec!["actions".to_string(), "body-markup".to_string()],))
                });
                let mut next_id = 0;
                builder.method(
                    "Notify",
                    (
                        "app", "replaces", "icon", "summary", "body", "actions", "hints", "expiry",
                    ),
                    ("id",),
                    move |context,
                          _,
                          (_, _, _, summary, body, actions, _, _): (
                        String,
                        u32,
                        String,
                        String,
                        String,
                        Vec<String>,
                        PropMap,
                        i32,
                    )| {
                        next_id += 1;
                        delivered
                            .send(Delivery {
                                sender: context.message().sender().unwrap().to_string(),
                                id: next_id,
                                summary,
                                body,
                                actions,
                            })
                            .unwrap();
                        // Let the test inspect cached reads while the transport is blocked.
                        released.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok((next_id,))
                    },
                );
                builder.method(
                    "CloseNotification",
                    ("id",),
                    (),
                    move |_, _, (id,): (u32,)| {
                        closed.send(id).unwrap();
                        Ok(())
                    },
                );
            });
            crossroads.insert(PATH, &[interface], ());
            connection.start_receive(
                dbus::message::MatchRule::new_method_call(),
                Box::new(move |message, connection| {
                    crossroads.handle_message(message, connection).unwrap();
                    true
                }),
            );
            ready.send(()).unwrap();
            while !stopped.load(Ordering::SeqCst) {
                connection.process(Duration::from_millis(20)).unwrap();
                for (destination, id, action, token) in invoked.try_iter() {
                    if let Some(token) = token {
                        send_token(&connection, &destination, id, &token);
                    }
                    send_action(&connection, &destination, id, &action);
                }
            }
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        Self {
            deliveries,
            closes,
            release,
            actions,
            stop,
            handle: Some(handle),
        }
    }

    pub fn release_delivery(&self) {
        self.release.send(()).unwrap();
    }
    pub fn invoke(&self, delivery: &Delivery) {
        self.actions
            .send((
                delivery.sender.clone(),
                delivery.id,
                "complete-setup".into(),
                None,
            ))
            .unwrap();
    }
    pub fn invoke_default_with_token(&self, delivery: &Delivery, token: &str) {
        self.actions
            .send((
                delivery.sender.clone(),
                delivery.id,
                "default".into(),
                Some(token.into()),
            ))
            .unwrap();
    }
}

pub fn send_token(connection: &Connection, destination: &str, id: u32, token: &str) {
    let mut message = Message::new_signal(PATH, INTERFACE, "ActivationToken")
        .unwrap()
        .append2(id, token.to_string());
    message.set_destination(Some(destination.into()));
    connection.send(message).unwrap();
    connection.channel().flush();
}

pub fn send_action(connection: &Connection, destination: &str, id: u32, action: &str) {
    let mut message = Message::new_signal(PATH, INTERFACE, "ActionInvoked")
        .unwrap()
        .append2(id, action.to_string());
    message.set_destination(Some(destination.into()));
    connection.send(message).unwrap();
    connection.channel().flush();
}

impl Drop for NotificationAgent {
    fn drop(&mut self) {
        let _ = self.release.send(());
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
