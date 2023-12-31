use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    sync::mpsc::{self, Receiver, TryRecvError, Sender},
    thread,
    time::{Duration, Instant, SystemTime},
};

use renet::{
    transport::{
        ClientAuthentication, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication, ServerConfig, NETCODE_USER_DATA_BYTES,
    },
    ConnectionConfig, DefaultChannel, RenetClient, RenetServer, ServerEvent,
};

use crate::Command;

// Helper struct to pass an username in the user data
struct Username(String);

impl Username {
    fn to_netcode_user_data(&self) -> [u8; NETCODE_USER_DATA_BYTES] {
        let mut user_data = [0u8; NETCODE_USER_DATA_BYTES];
        if self.0.len() > NETCODE_USER_DATA_BYTES - 8 {
            panic!("Username is too big");
        }
        user_data[0..8].copy_from_slice(&(self.0.len() as u64).to_le_bytes());
        user_data[8..self.0.len() + 8].copy_from_slice(self.0.as_bytes());

        user_data
    }

    fn from_user_data(user_data: &[u8; NETCODE_USER_DATA_BYTES]) -> Self {
        let mut buffer = [0u8; 8];
        buffer.copy_from_slice(&user_data[0..8]);
        let mut len = u64::from_le_bytes(buffer) as usize;
        len = len.min(NETCODE_USER_DATA_BYTES - 8);
        let data = user_data[8..len + 8].to_vec();
        let username = String::from_utf8(data).unwrap();
        Self(username)
    }
}

const PROTOCOL_ID: u64 = 7;

pub fn net_server(public_addr: SocketAddr) -> Sender<Command> {
    let (tx, rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();

    let connection_config = ConnectionConfig::default();
    let mut server: RenetServer = RenetServer::new(connection_config);

    let server_config = ServerConfig {
        max_clients: 64,
        protocol_id: PROTOCOL_ID,
        public_addr: public_addr,
        authentication: ServerAuthentication::Unsecure,
    };
    let socket: UdpSocket = UdpSocket::bind(public_addr).unwrap();

    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let mut transport = NetcodeServerTransport::new(current_time, server_config, socket).unwrap();

    let mut usernames: HashMap<u64, String> = HashMap::new();
    let mut received_messages = vec![];
    let mut last_updated = Instant::now();

    std::thread::spawn(move || {
        loop {
            let now = Instant::now();
            let duration = now - last_updated;
            last_updated = now;
    
            server.update(duration);
            transport.update(duration, &mut server).unwrap();
    
            received_messages.clear();
    
            while let Some(event) = server.get_event() {
                match event {
                    ServerEvent::ClientConnected { client_id } => {
                        let user_data = transport.user_data(client_id).unwrap();
                        let username = Username::from_user_data(&user_data);
                        usernames.insert(client_id, username.0);
                        println!("Client {} connected.", client_id)
                    }
                    ServerEvent::ClientDisconnected { client_id, reason } => {
                        println!("Client {} disconnected: {}", client_id, reason);
                        usernames.remove_entry(&client_id);
                    }
                }
            }
    
            for client_id in server.clients_id() {
                while let Some(message) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
                    let text = String::from_utf8(message.into()).unwrap();
                    let username = usernames.get(&client_id).unwrap();
                    println!("Client {} ({}) sent text: {}", username, client_id, text);
                    let text = format!("{}: {}", username, text);
                    received_messages.push(text);
                }
            }
    
            for text in received_messages.iter() {
                server.broadcast_message(DefaultChannel::ReliableOrdered, text.as_bytes().to_vec());
            }

            match rx.try_recv() {
                Ok(command) => {
                    println!("{:?}", command);
                    server.broadcast_message(DefaultChannel::ReliableOrdered, "asklog".as_bytes().to_vec());
                },
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => panic!("Channel disconnected"),
            }
    
            transport.send_packets(&mut server);
            thread::sleep(Duration::from_millis(50));
        }
    });

    tx
}

pub fn net_client(who: &str) {
    let server_addr: SocketAddr = format!("127.0.0.1:{}", 5000).parse().unwrap();
    let username = Username(who.to_string());
    println!("Usage: client 127.0.0.1:5000 CoolNickName");
    client(server_addr, username);
}

fn client(server_addr: SocketAddr, username: Username) {
    let connection_config = ConnectionConfig::default();
    let mut client = RenetClient::new(connection_config);

    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let client_id = current_time.as_millis() as u64;
    let authentication = ClientAuthentication::Unsecure {
        server_addr,
        client_id,
        user_data: Some(username.to_netcode_user_data()),
        protocol_id: PROTOCOL_ID,
    };

    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket).unwrap();
    let stdin_channel: Receiver<String> = spawn_stdin_channel();

    let mut last_updated = Instant::now();
    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        client.update(duration);
        transport.update(duration, &mut client).unwrap();

        if transport.is_connected() {
            match stdin_channel.try_recv() {
                Ok(text) => client.send_message(DefaultChannel::ReliableOrdered, text.as_bytes().to_vec()),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => panic!("Channel disconnected"),
            }

            while let Some(text) = client.receive_message(DefaultChannel::ReliableOrdered) {
                let text = String::from_utf8(text.into()).unwrap();
                println!("{}", text);
            }
        }

        transport.send_packets(&mut client).unwrap();
        thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_stdin_channel() -> Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || loop {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer).unwrap();
        tx.send(buffer.trim_end().to_string()).unwrap();
    });
    rx
}