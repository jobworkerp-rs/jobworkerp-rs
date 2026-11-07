//! TCP proxy in front of a test Redis that can drop the reply to a queue write,
//! to reproduce enqueue failures whose outcome is unknown to the caller.

use anyhow::Result;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

#[derive(Default)]
struct RedisProxyState {
    queue_key: Mutex<Option<Vec<u8>>>,
    drop_next_response: AtomicBool,
    reject_key_prefix: Mutex<Option<Vec<u8>>>,
}

pub struct RedisResponseDropProxy {
    url: String,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    state: Arc<RedisProxyState>,
}

impl RedisResponseDropProxy {
    pub async fn start(upstream_url: &str) -> Result<Self> {
        let upstream_addr = redis_tcp_address(upstream_url)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = redis_proxy_url(upstream_url, listener.local_addr()?)?;
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let state = Arc::new(RedisProxyState::default());
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                        continue;
                    }
                    accepted = listener.accept() => accepted,
                };
                let Ok((client, _)) = accepted else {
                    break;
                };
                let connection_state = task_state.clone();
                let connection_stop = shutdown_rx.clone();
                let upstream_addr = upstream_addr.clone();
                tokio::spawn(async move {
                    let _ = proxy_redis_connection(
                        client,
                        &upstream_addr,
                        connection_state,
                        connection_stop,
                    )
                    .await;
                });
            }
        });
        Ok(Self {
            url,
            shutdown,
            task,
            state,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Queue whose RPUSH reply `drop_next_queue_append_response` targets.
    pub fn observe_queue(&self, queue_key: &str) {
        *self.state.queue_key.lock().unwrap() = Some(queue_key.as_bytes().to_vec());
    }

    /// Forward the next RPUSH to the observed queue but close the client connection
    /// instead of returning Redis' reply, so the caller cannot tell it was applied.
    pub fn drop_next_queue_append_response(&self) {
        self.state.drop_next_response.store(true, Ordering::Release);
    }

    /// Answer the next command whose key starts with `prefix` with an error reply,
    /// without forwarding it to Redis.
    pub fn reject_next_command_on_key_prefix(&self, prefix: &str) {
        *self.state.reject_key_prefix.lock().unwrap() = Some(prefix.as_bytes().to_vec());
    }
}

impl Drop for RedisResponseDropProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

fn redis_tcp_address(redis_url: &str) -> Result<String> {
    let client = redis::Client::open(redis_url)?;
    match client.get_connection_info().addr() {
        redis::ConnectionAddr::Tcp(host, port) => Ok(format!("{host}:{port}")),
        other => anyhow::bail!("Redis test proxy supports plain TCP only, got {other}"),
    }
}

fn redis_proxy_url(redis_url: &str, proxy_addr: SocketAddr) -> Result<String> {
    let mut url = url::Url::parse(redis_url)?;
    let proxy_host = proxy_addr.ip().to_string();
    url.set_host(Some(&proxy_host))?;
    url.set_port(Some(proxy_addr.port()))
        .map_err(|_| anyhow::anyhow!("Unable to set Redis proxy URL port"))?;
    Ok(url.to_string())
}

async fn proxy_redis_connection(
    client: TcpStream,
    upstream_addr: &str,
    state: Arc<RedisProxyState>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let upstream = TcpStream::connect(upstream_addr).await?;
    let (mut client_reader, mut client_writer) = tokio::io::split(client);
    let (mut upstream_reader, mut upstream_writer) = tokio::io::split(upstream);
    loop {
        let command = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
            command = read_resp_command(&mut client_reader) => command?,
        };
        let Some(command) = command else {
            return Ok(());
        };
        let queue_key = state.queue_key.lock().unwrap().clone();
        let is_target_queue_append = queue_key.as_ref().is_some_and(|key| {
            command
                .args
                .first()
                .is_some_and(|arg| arg.eq_ignore_ascii_case(b"RPUSH"))
                && command.args.get(1) == Some(key)
        });
        let drop_response =
            is_target_queue_append && state.drop_next_response.swap(false, Ordering::AcqRel);
        let reject = {
            let mut prefix = state.reject_key_prefix.lock().unwrap();
            let matches = prefix.as_ref().is_some_and(|prefix| {
                command
                    .args
                    .get(1)
                    .is_some_and(|key| key.starts_with(prefix))
            });
            if matches {
                prefix.take();
            }
            matches
        };
        if reject {
            client_writer
                .write_all(b"-ERR injected by test proxy\r\n")
                .await?;
            continue;
        }

        upstream_writer.write_all(&command.raw).await?;
        let reply = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
            reply = read_resp_frame(&mut upstream_reader) => reply?,
        };

        if drop_response {
            return Ok(());
        }
        client_writer.write_all(&reply).await?;
    }
}

struct RespCommand {
    raw: Vec<u8>,
    args: Vec<Vec<u8>>,
}

async fn read_resp_command<R>(reader: &mut R) -> io::Result<Option<RespCommand>>
where
    R: AsyncRead + Unpin,
{
    let mut raw = Vec::new();
    let mut prefix = [0_u8; 1];
    match reader.read_exact(&mut prefix).await {
        Ok(_) => raw.push(prefix[0]),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    if prefix[0] != b'*' {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Redis commands must use RESP arrays",
        ));
    }
    let count_line = read_resp_line(reader, &mut raw).await?;
    let count = parse_resp_length(&count_line)?;
    if count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Redis command array cannot be null",
        ));
    }
    let mut args = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut argument_prefix = [0_u8; 1];
        reader.read_exact(&mut argument_prefix).await?;
        raw.push(argument_prefix[0]);
        if argument_prefix[0] != b'$' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Redis command arguments must use RESP bulk strings",
            ));
        }
        let length_line = read_resp_line(reader, &mut raw).await?;
        let length = parse_resp_length(&length_line)?;
        if length < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Redis command arguments cannot be null",
            ));
        }
        let mut argument = vec![0_u8; length as usize];
        reader.read_exact(&mut argument).await?;
        raw.extend_from_slice(&argument);
        let mut terminator = [0_u8; 2];
        reader.read_exact(&mut terminator).await?;
        raw.extend_from_slice(&terminator);
        if terminator != *b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Redis bulk string is missing its CRLF terminator",
            ));
        }
        args.push(argument);
    }
    Ok(Some(RespCommand { raw, args }))
}

fn read_resp_frame<'a, R>(reader: &'a mut R) -> futures::future::BoxFuture<'a, io::Result<Vec<u8>>>
where
    R: AsyncRead + Unpin + Send + 'a,
{
    Box::pin(async move {
        let mut frame = Vec::new();
        let mut prefix = [0_u8; 1];
        reader.read_exact(&mut prefix).await?;
        frame.push(prefix[0]);
        match prefix[0] {
            b'+' | b'-' | b':' | b',' | b'(' | b'#' | b'_' => {
                read_resp_line(reader, &mut frame).await?;
            }
            b'$' | b'!' | b'=' => {
                let length_line = read_resp_line(reader, &mut frame).await?;
                if length_line == b"?" {
                    loop {
                        let chunk_length_line = read_resp_line(reader, &mut frame).await?;
                        let length = parse_resp_length(&chunk_length_line)?;
                        if length == 0 {
                            break;
                        }
                        if length < 0 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "RESP chunk length cannot be negative",
                            ));
                        }
                        read_resp_bytes(reader, &mut frame, length as usize + 2).await?;
                    }
                } else {
                    let length = parse_resp_length(&length_line)?;
                    if length >= 0 {
                        read_resp_bytes(reader, &mut frame, length as usize + 2).await?;
                    }
                }
            }
            b'*' | b'~' | b'>' | b'%' | b'|' => {
                let count_line = read_resp_line(reader, &mut frame).await?;
                let count = parse_resp_length(&count_line)?;
                if count >= 0 {
                    let children = if matches!(prefix[0], b'%' | b'|') {
                        count.saturating_mul(2)
                    } else {
                        count
                    };
                    for _ in 0..children {
                        frame.extend_from_slice(&read_resp_frame(reader).await?);
                    }
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported RESP response prefix: {}", prefix[0]),
                ));
            }
        }
        Ok(frame)
    })
}

async fn read_resp_line<R>(reader: &mut R, frame: &mut Vec<u8>) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        reader.read_exact(&mut byte).await?;
        frame.push(byte[0]);
        if byte[0] == b'\n' {
            if line.last() != Some(&b'\r') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "RESP line is missing its CRLF terminator",
                ));
            }
            line.pop();
            return Ok(line);
        }
        line.push(byte[0]);
    }
}

async fn read_resp_bytes<R>(reader: &mut R, frame: &mut Vec<u8>, length: usize) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).await?;
    frame.extend_from_slice(&bytes);
    Ok(())
}

fn parse_resp_length(line: &[u8]) -> io::Result<i64> {
    std::str::from_utf8(line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .parse::<i64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::redis_proxy_url;
    use redis::{Client, ConnectionAddr};
    use std::net::SocketAddr;

    #[test]
    fn proxy_url_replaces_only_host_and_port() {
        let upstream_urls = [
            "redis://redis.example:6379/",
            "redis://user:password@redis.example:6379/7",
            "redis://ali%40ce:p%40ss%3Aword@redis.example:6379/12",
            "redis://:password@redis.example:6379/3",
            "redis://redis.example:6379/?protocol=resp3",
        ];
        let proxy_addr: SocketAddr = "127.0.0.1:54321".parse().unwrap();

        for upstream_url in upstream_urls {
            let upstream_client = Client::open(upstream_url).unwrap();
            let proxy_url = redis_proxy_url(upstream_url, proxy_addr).unwrap();
            let proxy_client = Client::open(proxy_url.as_str()).unwrap();

            assert_eq!(
                proxy_client.get_connection_info().addr(),
                &ConnectionAddr::Tcp("127.0.0.1".to_owned(), proxy_addr.port())
            );
            assert_eq!(
                proxy_client.get_connection_info().redis_settings().db(),
                upstream_client.get_connection_info().redis_settings().db()
            );
            assert_eq!(
                proxy_client
                    .get_connection_info()
                    .redis_settings()
                    .username(),
                upstream_client
                    .get_connection_info()
                    .redis_settings()
                    .username()
            );
            assert_eq!(
                proxy_client
                    .get_connection_info()
                    .redis_settings()
                    .password(),
                upstream_client
                    .get_connection_info()
                    .redis_settings()
                    .password()
            );
            assert_eq!(
                proxy_client
                    .get_connection_info()
                    .redis_settings()
                    .protocol(),
                upstream_client
                    .get_connection_info()
                    .redis_settings()
                    .protocol()
            );
        }
    }
}
