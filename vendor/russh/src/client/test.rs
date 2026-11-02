#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use log::debug;
    use ssh_key::PrivateKey;
    use tokio::net::TcpListener;

    use crate::cert::PublicKeyOrCertificate;
    // Import client types directly since we're in the client module
    use crate::Error;
    use crate::client::{Config, Handler, connect};
    use crate::keys::PrivateKeyWithHashAlg;
    use crate::server::{self, Auth, Handler as ServerHandler, Server, Session};
    use crate::{ChannelId, SshId}; // Import directly from crate root
    use rand::rng;

    #[derive(Clone)]
    struct TestServer {
        clients: Arc<Mutex<HashMap<(usize, ChannelId), server::Handle>>>,
        id: usize,
    }

    impl server::Server for TestServer {
        type Handler = Self;

        fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
            let s = self.clone();
            self.id += 1;
            s
        }
    }

    impl ServerHandler for TestServer {
        type Error = Error;

        async fn channel_open_session(
            &mut self,
            channel: crate::channels::Channel<server::Msg>,
            reply: server::ChannelOpenHandle,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            {
                let mut clients = self.clients.lock().unwrap();
                clients.insert((self.id, channel.id()), session.handle());
            }
            reply.accept().await;
            Ok(())
        }

        async fn auth_publickey(
            &mut self,
            _: &str,
            _: &ssh_key::PublicKey,
        ) -> Result<Auth, Self::Error> {
            debug!("auth_publickey");
            Ok(Auth::Accept)
        }

        async fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            debug!("server received data: {:?}", std::str::from_utf8(data));
            session.data(channel, data.to_vec())?;
            Ok(())
        }

        async fn tcpip_forward(
            &mut self,
            _: &str,
            _: &mut u32,
            _: &mut Session,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }

        async fn exec_request(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            session.channel_success(channel)?;
            if data == b"close" {
                session.close(channel)?;
            }
            if data == b"flood" {
                session.data(channel, vec![b'x'; 10_000])?;
            }
            Ok(())
        }
    }

    /// Counts `Handler::channel_close` calls
    struct ClosingClient {
        closes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Handler for ClosingClient {
        type Error = Error;

        async fn check_server_key(
            &mut self,
            _: &PublicKeyOrCertificate,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }

        async fn channel_close(
            &mut self,
            _: ChannelId,
            _: &mut crate::client::Session,
        ) -> Result<(), Self::Error> {
            self.closes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    async fn authenticated_session<H: Handler + Send + 'static>(
        handler: H,
    ) -> crate::client::Handle<H> {
        authenticated_session_with(Config::default(), handler).await
    }

    async fn authenticated_session_with<H: Handler + Send + 'static>(
        client_config: Config,
        handler: H,
    ) -> crate::client::Handle<H> {
        let client_key = PrivateKey::random(&mut rng(), ssh_key::Algorithm::Ed25519).unwrap();
        let mut config = server::Config::default();
        config.inactivity_timeout = None;
        config
            .keys
            .push(PrivateKey::random(&mut rng(), ssh_key::Algorithm::Ed25519).unwrap());
        let config = Arc::new(config);
        let mut server = TestServer {
            clients: Arc::new(Mutex::new(HashMap::new())),
            id: 0,
        };
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = socket.accept().await.unwrap();
            let server_handler = server.new_client(None);
            server::run_stream(config, socket, server_handler)
                .await
                .unwrap();
        });
        let mut session = connect(Arc::new(client_config), addr, handler)
            .await
            .unwrap();
        let auth_result = session
            .authenticate_publickey(
                "user",
                PrivateKeyWithHashAlg::new(Arc::new(client_key), None),
            )
            .await
            .unwrap();
        assert!(auth_result.success());
        session
    }

    #[test]
    fn test_locally_closed_record_is_consumed_once() {
        fn id(number: u32) -> ChannelId {
            use ssh_encoding::Decode;
            ChannelId::decode(&mut &number.to_be_bytes()[..]).unwrap()
        }
        let mut closed = crate::client::LocallyClosed::default();
        closed.mark(id(7));
        assert!(closed.take(id(7)));
        // A duplicate CLOSE from the peer is not taken for a second answer
        assert!(!closed.take(id(7)));
        // Nor is a CLOSE for a channel this side never closed, such as one still waiting for its open confirmation
        assert!(!closed.take(id(8)));
    }

    /// Wait for the granted bytes before checking that the channel stays quiet for 300 ms.
    async fn received_bytes(channel: &mut crate::Channel<crate::client::Msg>, expected: usize) -> usize {
        let mut total = 0;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while total < expected {
                match channel.wait().await {
                    Some(crate::ChannelMsg::Data { data }) => total += data.len(),
                    Some(_) => (),
                    None => panic!("channel ended after {total} of {expected} bytes"),
                }
            }
        })
        .await
        .expect("the granted bytes arrive within 30 s");
        while let Ok(Some(message)) =
            tokio::time::timeout(std::time::Duration::from_millis(300), channel.wait()).await
        {
            if let crate::ChannelMsg::Data { data } = message {
                total += data.len();
            }
        }
        total
    }

    #[tokio::test]
    async fn test_manual_window_grows_only_on_request() {
        let _ = env_logger::try_init();
        let config = Config {
            window_size: 4096,
            manual_window_adjust: true,
            ..Default::default()
        };
        let session = authenticated_session_with(config, Client {}).await;
        let mut channel = session.channel_open_session().await.unwrap();
        channel.exec(true, "flood").await.unwrap();
        // The peer may send only the initial window; nothing refills it on receipt.
        assert_eq!(received_bytes(&mut channel, 4096).await, 4096);
        session.adjust_window(channel.id(), 1000).await.unwrap();
        assert_eq!(received_bytes(&mut channel, 1000).await, 1000);
        session.adjust_window(channel.id(), 10_000).await.unwrap();
        assert_eq!(received_bytes(&mut channel, 10_000 - 5096).await, 10_000 - 5096);
    }

    #[tokio::test]
    async fn test_automatic_window_is_the_default() {
        let _ = env_logger::try_init();
        let config = Config {
            window_size: 4096,
            ..Default::default()
        };
        let session = authenticated_session_with(config, Client {}).await;
        let mut channel = session.channel_open_session().await.unwrap();
        channel.exec(true, "flood").await.unwrap();
        assert_eq!(received_bytes(&mut channel, 10_000).await, 10_000);
    }

    #[tokio::test]
    async fn test_local_close_receives_the_peer_close() {
        let _ = env_logger::try_init();
        let closes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let session = authenticated_session(ClosingClient {
            closes: closes.clone(),
        })
        .await;
        let mut channel = session.channel_open_session().await.unwrap();
        channel.close().await.unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), channel.wait())
            .await
            .expect("the peer's CLOSE reaches the channel");
        assert!(matches!(message, Some(crate::ChannelMsg::Close)));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), channel.wait())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_peer_close_still_reaches_the_channel_once() {
        let _ = env_logger::try_init();
        let closes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let session = authenticated_session(ClosingClient {
            closes: closes.clone(),
        })
        .await;
        let mut channel = session.channel_open_session().await.unwrap();
        channel.exec(true, "close").await.unwrap();
        let mut saw_close = false;
        while let Ok(Some(message)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), channel.wait()).await
        {
            saw_close |= matches!(message, crate::ChannelMsg::Close);
        }
        assert!(saw_close);
        // The client answers the peer's CLOSE; that answer must not be recorded as a local close
        channel.close().await.ok();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(closes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    struct Client {}

    impl Handler for Client {
        type Error = Error;

        async fn check_server_key(
            &mut self,
            _: &PublicKeyOrCertificate,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn test_client_connects_to_protocol_1_99() {
        let _ = env_logger::try_init();

        // Create a client key
        let client_key = PrivateKey::random(&mut rng(), ssh_key::Algorithm::Ed25519).unwrap();

        // Configure the server
        let mut config = server::Config::default();
        config.auth_rejection_time = std::time::Duration::from_secs(1);
        config.server_id = SshId::Standard("SSH-1.99-CustomServer_1.0".into());
        config.inactivity_timeout = None;
        config
            .keys
            .push(PrivateKey::random(&mut rng(), ssh_key::Algorithm::Ed25519).unwrap());
        let config = Arc::new(config);

        // Create server struct
        let mut server = TestServer {
            clients: Arc::new(Mutex::new(HashMap::new())),
            id: 0,
        };

        // Start the TCP listener for our mock server
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();

        // Spawn a separate task that will handle the server connection
        tokio::spawn(async move {
            // Accept a connection
            let (socket, _) = socket.accept().await.unwrap();

            // Handle the connection with the server
            let server_handler = server.new_client(None);
            server::run_stream(config, socket, server_handler)
                .await
                .unwrap();
        });

        println!("Server listening on {addr}");

        // Configure the client
        let client_config = Arc::new(Config::default());

        // Connect to the server
        let mut session = connect(client_config, addr, Client {}).await.unwrap();

        // Unfortunately, we can't directly verify the protocol version from the client API
        // The Protocol199Stream wrapper ensures the server sends SSH-1.99-CustomServer_1.0
        // The test passing means the client accepted this protocol version

        // Try to authenticate
        let auth_result = session
            .authenticate_publickey(
                std::env::var("USER").unwrap_or("user".to_string()),
                PrivateKeyWithHashAlg::new(
                    Arc::new(client_key),
                    session.best_supported_rsa_hash().await.unwrap().flatten(),
                ),
            )
            .await
            .unwrap();

        assert!(auth_result.success());

        // Try opening a session channel
        let mut channel = session.channel_open_session().await.unwrap();

        // Send some data
        let test_data = b"Hello, 1.99 protocol server!";
        channel.data(&test_data[..]).await.unwrap();

        // Wait for response
        let msg = channel.wait().await.unwrap();
        match msg {
            crate::channels::ChannelMsg::Data { data: msg_data } => {
                assert_eq!(test_data.as_slice(), &msg_data[..]);
            }
            msg => panic!("Unexpected message {msg:?}"),
        }
    }

    #[tokio::test]
    async fn test_send_custom_global_request() {
        let _ = env_logger::try_init();

        let client_key = PrivateKey::random(&mut rng(), ssh_key::Algorithm::Ed25519).unwrap();

        let mut config = server::Config::default();
        config.auth_rejection_time = std::time::Duration::from_secs(1);
        config.inactivity_timeout = None;
        config
            .keys
            .push(PrivateKey::random(&mut rng(), ssh_key::Algorithm::Ed25519).unwrap());
        let config = Arc::new(config);

        let mut server = TestServer {
            clients: Arc::new(Mutex::new(HashMap::new())),
            id: 0,
        };

        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();

        tokio::spawn(async move {
            let (socket, _) = socket.accept().await.unwrap();
            let server_handler = server.new_client(None);
            server::run_stream(config, socket, server_handler)
                .await
                .unwrap();
        });

        let client_config = Arc::new(Config::default());
        let mut session = connect(client_config, addr, Client {}).await.unwrap();

        let auth_result = session
            .authenticate_publickey(
                std::env::var("USER").unwrap_or("user".to_string()),
                PrivateKeyWithHashAlg::new(
                    Arc::new(client_key),
                    session.best_supported_rsa_hash().await.unwrap().flatten(),
                ),
            )
            .await
            .unwrap();
        assert!(auth_result.success());

        // The default server has no handler for this request name, so it replies
        // with a failure, which surfaces as RequestDenied.
        let denied = session
            .send_global_request("custom-request@example.com", b"payload", true)
            .await;
        assert!(matches!(denied, Err(Error::RequestDenied)));

        // Without a reply requested the call returns immediately and the server
        // must stay silent. Queue a reply-wanting request right behind it: an
        // unsolicited failure for the first request would be matched to the
        // forward and turn its success into RequestDenied.
        let no_reply = session
            .send_global_request("custom-request@example.com", b"payload", false)
            .await
            .unwrap();
        assert!(no_reply.is_none());
        session.tcpip_forward("127.0.0.1", 12345).await.unwrap();
    }
}
