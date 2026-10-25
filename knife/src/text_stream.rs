//! Optional WebSocket text stream for text hookers (`--text-websocket HOST:PORT`), as in shin. Each connected
//! client receives every displayed message and every choice list as one plain-text message, with the script
//! markup removed. A client that connects late first receives the most recent text.
use std::{
    io::ErrorKind,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use tracing::{info, warn};
use tungstenite::Message;

use crate::text::{Run, parse_markup};

/// How often a client thread checks for queued text between reads.
const POLL: Duration = Duration::from_millis(50);
/// A client that stops reading is dropped instead of stalling its thread.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

static STREAM: OnceLock<Arc<TextStream>> = OnceLock::new();

/// Start the process-wide server and return its bound address. Text sent before this is ignored.
pub fn start(address: &str) -> Result<SocketAddr> {
    let (stream, local) = TextStream::bind(address)?;
    STREAM.set(stream).map_err(|_| anyhow!("The text WebSocket server is already running"))?;
    info!(%local, "Text WebSocket server listening");
    Ok(local)
}

/// A message as it appears in the window. `id` identifies the script line: the same message shown again right
/// after itself (a page restored by a load or rewind) is sent once.
pub fn message(text: &str, id: u64) {
    if let Some(stream) = STREAM.get() {
        stream.broadcast(Some(id), &plain_text(text));
    }
}

/// The options of a choice, one per line.
pub fn choices<'a>(options: impl IntoIterator<Item = &'a str>) {
    if let Some(stream) = STREAM.get() {
        let lines: Vec<String> = options.into_iter().map(plain_text).filter(|s| !s.is_empty()).collect();
        stream.broadcast(None, &lines.join("\n"));
    }
}

/// Displayed characters only: ruby readings and control codes are dropped, inline glyph tags become Unicode
/// look-alikes (`text::tag_symbol`) and bar tags the U+3000 the game puts under the bar, `@n` / `@r` / `^`
/// newlines, and surrounding blank lines are trimmed.
pub fn plain_text(text: &str) -> String {
    let mut output = String::new();
    for run in parse_markup(text) {
        match run {
            Run::Text(s) | Run::Bar { text: s, .. } => output.extend(s.chars().map(crate::text::tag_symbol)),
            Run::Ruby { base, .. } => output.push_str(&base),
            Run::Newline => output.push('\n'),
            Run::Italic | Run::Wobble(_) | Run::Plain | Run::Fill(_) | Run::Edge(_) | Run::Indent => {}
        }
    }
    output.trim_matches(|c| c == '\n' || c == '\r').to_owned()
}

#[derive(Default)]
struct Clients {
    senders: Vec<mpsc::Sender<Arc<str>>>,
    last: Option<Arc<str>>,
    /// The message id of `last`, when it was a message.
    last_message: Option<u64>,
}

#[derive(Default)]
pub struct TextStream {
    clients: Mutex<Clients>,
}

impl TextStream {
    fn bind(address: &str) -> Result<(Arc<Self>, SocketAddr)> {
        let listener =
            TcpListener::bind(address).with_context(|| format!("Binding the text WebSocket server to {address}"))?;
        let local = listener.local_addr()?;
        let stream = Arc::new(Self::default());
        let accepting = stream.clone();
        std::thread::Builder::new().name("text-websocket".into()).spawn(move || {
            for incoming in listener.incoming() {
                let tcp = match incoming {
                    Ok(tcp) => tcp,
                    Err(error) => {
                        warn!(%error, "Text WebSocket accept failed");
                        continue;
                    }
                };
                let stream = accepting.clone();
                let spawned = std::thread::Builder::new().name("text-websocket-client".into()).spawn(move || {
                    let peer = tcp.peer_addr().ok();
                    match stream.serve(tcp) {
                        Ok(()) => info!(?peer, "Text WebSocket client disconnected"),
                        Err(error) => info!(?peer, %error, "Text WebSocket client dropped"),
                    }
                });
                if let Err(error) = spawned {
                    warn!(%error, "Text WebSocket client thread failed to start");
                }
            }
        })?;
        Ok((stream, local))
    }

    fn broadcast(&self, message_id: Option<u64>, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut clients = self.clients.lock().unwrap();
        if message_id.is_some() && clients.last_message == message_id && clients.last.as_deref() == Some(text) {
            return;
        }
        let text: Arc<str> = text.into();
        clients.last = Some(text.clone());
        clients.last_message = message_id;
        clients.senders.retain(|sender| sender.send(text.clone()).is_ok());
    }

    fn serve(&self, tcp: TcpStream) -> Result<()> {
        tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let mut socket = tungstenite::accept(tcp).context("WebSocket handshake")?;
        info!(peer = ?socket.get_ref().peer_addr().ok(), "Text WebSocket client connected");
        socket.get_ref().set_read_timeout(Some(POLL))?;
        let (sender, receiver) = mpsc::channel();
        {
            let mut clients = self.clients.lock().unwrap();
            if let Some(last) = &clients.last {
                sender.send(last.clone()).unwrap();
            }
            clients.senders.push(sender);
        }
        loop {
            loop {
                match receiver.try_recv() {
                    Ok(text) => socket.send(Message::text(text.as_ref()))?,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            // Reads answer pings and notice closes; client text is ignored.
            match socket.read() {
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_drops_markup_and_ruby() {
        assert_eq!(plain_text("先ほど<前触|まえぶ>れもなく@k"), "先ほど前触れもなく");
        assert_eq!(plain_text("@r一行目^二行目[ber00][ber00]@n"), "一行目\n二行目\u{3000}\u{3000}");
        assert_eq!(plain_text("friend.[heart] [x]"), "friend.♥ [x]");
        // `@c` takes one character, `@r` is no line break
        assert_eq!(plain_text("@c1赤い@r字@c9"), "赤い字");
    }

    fn read(client: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>) -> String {
        match client.read().unwrap() {
            Message::Text(text) => text.to_string(),
            other => panic!("unexpected frame {other:?}"),
        }
    }

    #[test]
    fn every_client_receives_broadcasts_and_late_clients_get_the_last_text() {
        let (stream, local) = TextStream::bind("127.0.0.1:0").unwrap();
        stream.broadcast(None, "");
        stream.broadcast(None, "一行目");
        let url = format!("ws://{local}");
        let (mut first, _) = tungstenite::connect(&url).unwrap();
        assert_eq!(read(&mut first), "一行目");
        let (mut second, _) = tungstenite::connect(&url).unwrap();
        assert_eq!(read(&mut second), "一行目");
        while stream.clients.lock().unwrap().senders.len() < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        stream.broadcast(Some(7), "二行目\n続き");
        // the same message shown again is not repeated
        stream.broadcast(Some(7), "二行目\n続き");
        assert_eq!(read(&mut first), "二行目\n続き");
        assert_eq!(read(&mut second), "二行目\n続き");
        first.close(None).unwrap();
        while stream.clients.lock().unwrap().senders.len() > 1 {
            stream.broadcast(None, "三行目");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(read(&mut second), "三行目");
    }
}
