use crate::config::Edge;
use ring::rand::SecureRandom;
use ring::{aead, hkdf, hmac, rand};
use std::convert::TryInto;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"DSKBRG01";
const PROTOCOL_VERSION: u16 = 1;
const HELLO_SIZE: usize = 8 + 2 + 32 + 32 + 32;
const MAX_FRAME_SIZE: usize = 64;
const IO_DEADLINE: Duration = Duration::from_secs(3);
const WRITE_DEADLINE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireMessage {
    Activate { entry_edge: Edge },
    ActivateAck,
    MouseMove { dx: i32, dy: i32 },
    MouseButton { button: u8, down: bool },
    Wheel { horizontal: i32, vertical: i32 },
    Key { usage: u16, down: bool },
    ReturnControl,
    ReleaseAll,
    Ping(u64),
    Pong(u64),
}

impl WireMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(12);
        match self {
            Self::Activate { entry_edge } => {
                output.push(1);
                output.push(entry_edge.as_u8());
            }
            Self::ActivateAck => output.push(2),
            Self::MouseMove { dx, dy } => {
                output.push(3);
                output.extend_from_slice(&dx.to_be_bytes());
                output.extend_from_slice(&dy.to_be_bytes());
            }
            Self::MouseButton { button, down } => {
                output.push(4);
                output.push(*button);
                output.push(u8::from(*down));
            }
            Self::Wheel {
                horizontal,
                vertical,
            } => {
                output.push(5);
                output.extend_from_slice(&horizontal.to_be_bytes());
                output.extend_from_slice(&vertical.to_be_bytes());
            }
            Self::Key { usage, down } => {
                output.push(6);
                output.extend_from_slice(&usage.to_be_bytes());
                output.push(u8::from(*down));
            }
            Self::ReturnControl => output.push(7),
            Self::ReleaseAll => output.push(8),
            Self::Ping(value) => {
                output.push(9);
                output.extend_from_slice(&value.to_be_bytes());
            }
            Self::Pong(value) => {
                output.push(10);
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
        output
    }

    pub fn decode(input: &[u8]) -> io::Result<Self> {
        let kind = *input.first().ok_or_else(|| invalid_data("empty message"))?;
        match kind {
            1 if input.len() == 2 => {
                let edge = Edge::from_u8(input[1]);
                if edge == Edge::Disabled {
                    return Err(invalid_data("invalid activation edge"));
                }
                Ok(Self::Activate { entry_edge: edge })
            }
            2 if input.len() == 1 => Ok(Self::ActivateAck),
            3 if input.len() == 9 => Ok(Self::MouseMove {
                dx: i32::from_be_bytes(input[1..5].try_into().unwrap()),
                dy: i32::from_be_bytes(input[5..9].try_into().unwrap()),
            }),
            4 if input.len() == 3 && input[2] <= 1 => Ok(Self::MouseButton {
                button: input[1],
                down: input[2] == 1,
            }),
            5 if input.len() == 9 => Ok(Self::Wheel {
                horizontal: i32::from_be_bytes(input[1..5].try_into().unwrap()),
                vertical: i32::from_be_bytes(input[5..9].try_into().unwrap()),
            }),
            6 if input.len() == 4 && input[3] <= 1 => Ok(Self::Key {
                usage: u16::from_be_bytes(input[1..3].try_into().unwrap()),
                down: input[3] == 1,
            }),
            7 if input.len() == 1 => Ok(Self::ReturnControl),
            8 if input.len() == 1 => Ok(Self::ReleaseAll),
            9 if input.len() == 9 => Ok(Self::Ping(u64::from_be_bytes(
                input[1..9].try_into().unwrap(),
            ))),
            10 if input.len() == 9 => Ok(Self::Pong(u64::from_be_bytes(
                input[1..9].try_into().unwrap(),
            ))),
            _ => Err(invalid_data("unknown or malformed message")),
        }
    }
}

pub struct SecureWriter {
    stream: TcpStream,
    key: aead::LessSafeKey,
    sequence: u64,
}

pub struct SecureReader {
    stream: TcpStream,
    key: aead::LessSafeKey,
    sequence: u64,
}

pub struct Session {
    pub peer_id: String,
    pub reader: SecureReader,
    pub writer: SecureWriter,
}

impl SecureWriter {
    pub fn send(&mut self, message: &WireMessage) -> io::Result<()> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid_data("sequence exhausted"))?;
        let sequence_bytes = self.sequence.to_be_bytes();
        let nonce = nonce_for(self.sequence);
        let payload = message.encode();
        let mut frame = Vec::with_capacity(12 + payload.len() + aead::MAX_TAG_LEN);
        frame.extend_from_slice(&[0_u8; 4]);
        frame.extend_from_slice(&sequence_bytes);
        frame.extend_from_slice(&payload);
        let tag = self
            .key
            .seal_in_place_separate_tag(
                nonce,
                aead::Aad::from(sequence_bytes.as_slice()),
                &mut frame[12..],
            )
            .map_err(|_| invalid_data("frame encryption failed"))?;
        frame.extend_from_slice(tag.as_ref());
        let frame_len = frame
            .len()
            .checked_sub(4)
            .ok_or_else(|| invalid_data("frame too large"))?;
        if frame_len > MAX_FRAME_SIZE {
            return Err(invalid_data("frame too large"));
        }
        frame[..4].copy_from_slice(&(frame_len as u32).to_be_bytes());
        write_all_until(&mut self.stream, &frame, Instant::now() + WRITE_DEADLINE)
    }
}

impl SecureReader {
    pub fn receive(&mut self) -> io::Result<WireMessage> {
        let deadline = Instant::now() + IO_DEADLINE;
        let mut length_bytes = [0_u8; 4];
        read_exact_until(&mut self.stream, &mut length_bytes, deadline)?;
        let frame_len = u32::from_be_bytes(length_bytes) as usize;
        if !(8 + aead::MAX_TAG_LEN..=MAX_FRAME_SIZE).contains(&frame_len) {
            return Err(invalid_data("invalid frame length"));
        }

        let mut sequence_bytes = [0_u8; 8];
        read_exact_until(&mut self.stream, &mut sequence_bytes, deadline)?;
        let sequence = u64::from_be_bytes(sequence_bytes);
        let expected = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid_data("sequence exhausted"))?;
        if sequence != expected {
            return Err(invalid_data("out-of-order or replayed frame"));
        }

        let mut sealed = vec![0_u8; frame_len - 8];
        read_exact_until(&mut self.stream, &mut sealed, deadline)?;
        let plaintext = self
            .key
            .open_in_place(
                nonce_for(sequence),
                aead::Aad::from(sequence_bytes.as_slice()),
                &mut sealed,
            )
            .map_err(|_| invalid_data("frame authentication failed"))?;
        let message = WireMessage::decode(plaintext)?;
        self.sequence = sequence;
        Ok(message)
    }
}

pub fn client_handshake(
    mut stream: TcpStream,
    node_id: &str,
    pairing_key: &[u8; 32],
) -> io::Result<Session> {
    configure_stream(&stream)?;
    let client_nonce = random_nonce()?;
    let client_id = node_id_bytes(node_id)?;
    let client_unsigned = hello_unsigned(&client_id, &client_nonce);
    let auth_key = hmac::Key::new(hmac::HMAC_SHA256, pairing_key);
    let mut hello = client_unsigned.clone();
    hello.extend_from_slice(hmac::sign(&auth_key, &client_unsigned).as_ref());
    write_all_until(&mut stream, &hello, Instant::now() + WRITE_DEADLINE)?;

    let mut response = [0_u8; HELLO_SIZE];
    read_exact_until(&mut stream, &mut response, Instant::now() + IO_DEADLINE)?;
    let (peer_id, server_nonce) = verify_server_hello(&response, &client_nonce, &auth_key)?;
    let (client_to_server, server_to_client) =
        derive_keys(pairing_key, &client_nonce, &server_nonce)?;
    let reader_stream = stream.try_clone()?;
    Ok(Session {
        peer_id,
        reader: SecureReader {
            stream: reader_stream,
            key: aead_key(&server_to_client)?,
            sequence: 0,
        },
        writer: SecureWriter {
            stream,
            key: aead_key(&client_to_server)?,
            sequence: 0,
        },
    })
}

pub fn server_handshake(
    mut stream: TcpStream,
    node_id: &str,
    pairing_key: &[u8; 32],
) -> io::Result<Session> {
    configure_stream(&stream)?;
    let auth_key = hmac::Key::new(hmac::HMAC_SHA256, pairing_key);
    let mut hello = [0_u8; HELLO_SIZE];
    read_exact_until(&mut stream, &mut hello, Instant::now() + IO_DEADLINE)?;
    let (peer_id, client_nonce) = verify_client_hello(&hello, &auth_key)?;

    let server_nonce = random_nonce()?;
    let server_id = node_id_bytes(node_id)?;
    let unsigned = server_hello_unsigned(&server_id, &server_nonce, &client_nonce);
    let mut response = hello_unsigned(&server_id, &server_nonce);
    response.extend_from_slice(hmac::sign(&auth_key, &unsigned).as_ref());
    write_all_until(&mut stream, &response, Instant::now() + WRITE_DEADLINE)?;

    let (client_to_server, server_to_client) =
        derive_keys(pairing_key, &client_nonce, &server_nonce)?;
    let reader_stream = stream.try_clone()?;
    Ok(Session {
        peer_id,
        reader: SecureReader {
            stream: reader_stream,
            key: aead_key(&client_to_server)?,
            sequence: 0,
        },
        writer: SecureWriter {
            stream,
            key: aead_key(&server_to_client)?,
            sequence: 0,
        },
    })
}

fn verify_client_hello(
    hello: &[u8; HELLO_SIZE],
    auth_key: &hmac::Key,
) -> io::Result<(String, [u8; 32])> {
    verify_common_header(hello)?;
    hmac::verify(
        auth_key,
        &hello[..HELLO_SIZE - 32],
        &hello[HELLO_SIZE - 32..],
    )
    .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "pairing key rejected"))?;
    Ok(read_hello_identity_and_nonce(hello))
}

fn verify_server_hello(
    hello: &[u8; HELLO_SIZE],
    client_nonce: &[u8; 32],
    auth_key: &hmac::Key,
) -> io::Result<(String, [u8; 32])> {
    verify_common_header(hello)?;
    let mut signed = server_hello_unsigned(
        hello[10..42].try_into().unwrap(),
        hello[42..74].try_into().unwrap(),
        client_nonce,
    );
    hmac::verify(auth_key, &signed, &hello[HELLO_SIZE - 32..])
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "pairing key rejected"))?;
    signed.fill(0);
    Ok(read_hello_identity_and_nonce(hello))
}

fn verify_common_header(hello: &[u8; HELLO_SIZE]) -> io::Result<()> {
    if &hello[..8] != MAGIC
        || u16::from_be_bytes(hello[8..10].try_into().unwrap()) != PROTOCOL_VERSION
    {
        return Err(invalid_data("incompatible DeskBridge protocol"));
    }
    Ok(())
}

fn read_hello_identity_and_nonce(hello: &[u8; HELLO_SIZE]) -> (String, [u8; 32]) {
    let peer_id = String::from_utf8_lossy(&hello[10..42]).to_string();
    let nonce = hello[42..74].try_into().unwrap();
    (peer_id, nonce)
}

fn hello_unsigned(node_id: &[u8; 32], nonce: &[u8; 32]) -> Vec<u8> {
    let mut output = Vec::with_capacity(HELLO_SIZE - 32);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    output.extend_from_slice(node_id);
    output.extend_from_slice(nonce);
    output
}

fn server_hello_unsigned(
    node_id: &[u8; 32],
    server_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
) -> Vec<u8> {
    let mut output = b"server-response".to_vec();
    output.extend_from_slice(client_nonce);
    output.extend_from_slice(&hello_unsigned(node_id, server_nonce));
    output
}

fn derive_keys(
    pairing_key: &[u8; 32],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> io::Result<([u8; 32], [u8; 32])> {
    let mut salt_bytes = [0_u8; 64];
    salt_bytes[..32].copy_from_slice(client_nonce);
    salt_bytes[32..].copy_from_slice(server_nonce);
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt_bytes);
    let pseudo_random_key = salt.extract(pairing_key);
    let client_info = [b"DeskBridge v1 client to server".as_slice()];
    let server_info = [b"DeskBridge v1 server to client".as_slice()];
    let mut client_key = [0_u8; 32];
    let mut server_key = [0_u8; 32];
    pseudo_random_key
        .expand(&client_info, KeyLength32)
        .map_err(|_| invalid_data("key derivation failed"))?
        .fill(&mut client_key)
        .map_err(|_| invalid_data("key derivation failed"))?;
    pseudo_random_key
        .expand(&server_info, KeyLength32)
        .map_err(|_| invalid_data("key derivation failed"))?
        .fill(&mut server_key)
        .map_err(|_| invalid_data("key derivation failed"))?;
    Ok((client_key, server_key))
}

#[derive(Clone, Copy)]
struct KeyLength32;

impl hkdf::KeyType for KeyLength32 {
    fn len(&self) -> usize {
        32
    }
}

fn aead_key(bytes: &[u8; 32]) -> io::Result<aead::LessSafeKey> {
    let key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, bytes)
        .map_err(|_| invalid_data("invalid encryption key"))?;
    Ok(aead::LessSafeKey::new(key))
}

fn nonce_for(sequence: u64) -> aead::Nonce {
    let mut nonce = [0_u8; 12];
    nonce[4..].copy_from_slice(&sequence.to_be_bytes());
    aead::Nonce::assume_unique_for_key(nonce)
}

fn random_nonce() -> io::Result<[u8; 32]> {
    let mut nonce = [0_u8; 32];
    rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| io::Error::other("system random generator failed"))?;
    Ok(nonce)
}

fn node_id_bytes(node_id: &str) -> io::Result<[u8; 32]> {
    node_id
        .as_bytes()
        .try_into()
        .map_err(|_| invalid_data("node id must contain 32 ASCII bytes"))
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(IO_DEADLINE))?;
    stream.set_write_timeout(Some(WRITE_DEADLINE))?;
    Ok(())
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut buffer: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !buffer.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "frame deadline exceeded"))?;
        stream.set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        match stream.read(buffer) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(read) => {
                let (_, rest) = buffer.split_at_mut(read);
                buffer = rest;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_until(stream: &mut TcpStream, mut buffer: &[u8], deadline: Instant) -> io::Result<()> {
    while !buffer.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "write deadline exceeded"))?;
        stream.set_write_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        match stream.write(buffer) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(written) => buffer = &buffer[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn messages_round_trip() {
        let messages = [
            WireMessage::Activate {
                entry_edge: Edge::Left,
            },
            WireMessage::MouseMove { dx: -42, dy: 900 },
            WireMessage::MouseButton {
                button: 2,
                down: true,
            },
            WireMessage::Wheel {
                horizontal: -1,
                vertical: 120,
            },
            WireMessage::Key {
                usage: 0x04,
                down: false,
            },
            WireMessage::ReleaseAll,
        ];
        for message in messages {
            assert_eq!(WireMessage::decode(&message.encode()).unwrap(), message);
        }
    }

    #[test]
    fn encrypted_session_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let key = [7_u8; 32];
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut session =
                server_handshake(stream, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &key).unwrap();
            let received = session.reader.receive().unwrap();
            assert_eq!(received, WireMessage::MouseMove { dx: 12, dy: -8 });
            session.writer.send(&WireMessage::ActivateAck).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        let mut session =
            client_handshake(stream, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &key).unwrap();
        assert_eq!(session.peer_id, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        session
            .writer
            .send(&WireMessage::MouseMove { dx: 12, dy: -8 })
            .unwrap();
        assert_eq!(session.reader.receive().unwrap(), WireMessage::ActivateAck);
        server.join().unwrap();
    }
}
