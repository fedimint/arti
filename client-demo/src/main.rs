//! A minimal client for connecting to the tor network
//!
//! Right now, all the client does is load a directory from disk, and
//! launch an authenticated handshake.
//!
//! It expects to find a local chutney network, or a cached tor
//! directory.

#![warn(missing_docs)]

mod err;

use argh::FromArgs;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use futures::lock::Mutex;
use futures::stream::StreamExt;
use futures::task::SpawnError;
use log::{info, warn, LevelFilter};
use std::path::PathBuf;
use std::sync::Arc;

use tor_chanmgr::transport::nativetls::NativeTlsTransport;
use tor_proto::circuit::ClientCirc;

use err::{Error, Result};

#[derive(FromArgs)]
/// Make a connection to the Tor network, connect to
/// www.torproject.org, and see a redirect page. Requires a tor
/// directory cache, or running chutney network.
///
/// This is a demo; you get no stability guarantee.
struct Args {
    /// where to find a tor directory cache.  Why not try ~/.tor?
    #[argh(option)]
    tor_dir: Option<PathBuf>,
    /// where to find a chutney directory.
    #[argh(option)]
    chutney_dir: Option<PathBuf>,
    /// how many times to repeat the test
    #[argh(option, default = "1")]
    n: usize,
    /// try doing a flooding test (to 127.0.0.1:9999)? Requires chutney.
    #[argh(switch)]
    flood: bool,
    /// try doing a download test (to 127.0.0.1:9999)? Requires chutney.
    #[argh(switch)]
    dl: bool,
    /// enable trace-level logging
    #[argh(switch)]
    trace: bool,
    /// run a socks proxy on port N. [WILL NOT WORK YET]
    #[argh(option)]
    socksport: Option<u16>,
}

struct Spawner {
    name: String,
}

impl Spawner {
    fn new(name: &str) -> Self {
        Spawner {
            name: name.to_string(),
        }
    }
}

impl futures::task::Spawn for Spawner {
    fn spawn_obj(
        &self,
        future: futures::task::FutureObj<'static, ()>,
    ) -> std::result::Result<(), SpawnError> {
        use async_std::task::Builder;
        let builder = Builder::new().name(self.name.clone());
        let _handle = builder.spawn(future).map_err(|_| SpawnError::shutdown())?;
        Ok(())
    }
}

async fn test_cat(mut circ: ClientCirc) -> Result<()> {
    let stream = circ.begin_stream("127.0.0.1", 9999).await?;
    for x in 1..2000 {
        let one_k = [b'x'; 1024];
        stream.write_bytes(&one_k[..]).await?;
        dbg!(x);
    }
    dbg!("done");
    Ok(())
}

async fn test_dl(mut circ: ClientCirc) -> Result<()> {
    let mut stream = circ.begin_stream("127.0.0.1", 9999).await?;
    let mut n_read = 0;
    let mut buf = [0u8; 512];
    while let Ok(n) = stream.read_bytes(&mut buf[..]).await {
        if n == 0 {
            dbg!("Closed, apparently.");
        }
        n_read += n;
        dbg!(n_read);
        if n_read >= 1000000 {
            dbg!(n_read);
            break;
        }
    }
    dbg!("done?");
    Ok(())
}

async fn test_http(mut circ: ClientCirc) -> Result<()> {
    let mut stream = circ.begin_stream("www.torproject.org", 80).await?;

    let request = b"GET / HTTP/1.0\r\nHost: www.torproject.org\r\n\r\n";

    stream.write_bytes(&request[..]).await?;

    let mut buf = [0u8; 512];
    while let Ok(n) = stream.read_bytes(&mut buf[..]).await {
        if n == 0 {
            break;
        }
        let msg = &buf[..n];
        // XXXX this will crash on bad utf-8
        println!("{}", std::str::from_utf8(msg).unwrap());
    }
    Ok(())
}

/// Load a network directory from `~/src/chutney/net/nodes/000a/`
fn get_netdir(args: &Args) -> Result<tor_netdir::NetDir> {
    if args.tor_dir.is_some() && args.chutney_dir.is_some() {
        eprintln!("Can't specify both --tor-dir and --chutney-dir");
        return Err(Error::Misc("arguments"));
    }
    let mut cfg = tor_netdir::NetDirConfig::new();

    if let Some(ref d) = args.tor_dir {
        cfg.add_default_authorities();
        cfg.set_cache_path(&d);
    } else if let Some(ref d) = args.chutney_dir {
        cfg.add_authorities_from_chutney(&d)?;
        cfg.set_cache_path(&d);
    } else {
        eprintln!("Must specify --tor-dir or --chutney-dir");
        return Err(Error::Misc("arguments"));
    }

    Ok(cfg.load()?)
}

async fn handle_socks_conn(
    dir: Arc<tor_netdir::NetDir>,
    circmgr: Arc<tor_circmgr::CircMgr<NativeTlsTransport>>,
    stream: async_std::net::TcpStream,
) -> Result<()> {
    let mut handshake = tor_socks::SocksHandshake::new();

    let (mut r, mut w) = stream.split();
    let mut inbuf = [0_u8; 1024];
    let mut n_read = 0;
    let request = loop {
        // Read some more stuff.
        n_read += r.read(&mut inbuf[n_read..]).await?;

        // try to advance the handshake.
        let action = match handshake.handshake(&inbuf[..n_read]) {
            Err(tor_socks::Error::Truncated) => continue,
            Err(e) => return Err(e.into()),
            Ok(action) => action,
        };

        // reply if needed.
        if action.drain > 0 {
            (&mut inbuf).copy_within(action.drain..action.drain + n_read, 0);
            n_read -= action.drain;
        }
        if !action.reply.is_empty() {
            w.write(&action.reply[..]).await?;
        }
        if action.finished {
            break handshake.into_request();
        }
    }
    .unwrap();

    let addr = request.addr().to_string();
    let port = request.port();
    info!("Got a socks request for {}:{}", addr, port);

    let exit_ports = [port];
    let mut circ = circmgr
        .get_or_launch_exit(dir.as_ref(), &exit_ports)
        .await?;
    info!("Got a circuit for {}:{}", addr, port);

    let stream = circ.begin_stream(&addr, port).await?;
    info!("Got a stream for {}:{}", addr, port);
    let reply = request.reply(tor_socks::SocksStatus::SUCCEEDED, None);
    w.write(&reply[..]).await?;

    let stream = Arc::new(Mutex::new(stream));
    let stream2 = Arc::clone(&stream);

    // XXXX This won't work: we're going to hit a deadlock since the writing
    // XXXX thread will block while waiting for the reading thread to have
    // XXXX something to say.
    let _t1 = async_std::task::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            dbg!("read?");
            let n = match r.read(&mut buf[..]).await {
                Err(e) => break e.into(),
                Ok(n) => n,
            };
            dbg!(n);
            if let Err(e) = stream.lock().await.write_bytes(&buf[..n]).await {
                break e;
            }
        }
    });
    let _t2 = async_std::task::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            dbg!("write?");
            let n = match stream2.lock().await.read_bytes(&mut buf[..]).await {
                Err(e) => break e,
                Ok(n) => n,
            };
            dbg!(n);
            if let Err(e) = w.write(&buf[..n]).await {
                break e.into();
            }
        }
    });

    Ok(())
}

async fn run_socks_proxy(
    dir: tor_netdir::NetDir,
    circmgr: tor_circmgr::CircMgr<NativeTlsTransport>,
    args: Args,
) -> Result<()> {
    let dir = Arc::new(dir);
    let circmgr = Arc::new(circmgr);
    let listener =
        async_std::net::TcpListener::bind(("localhost", args.socksport.unwrap())).await?;
    let mut incoming = listener.incoming();

    while let Some(stream) = incoming.next().await {
        let stream = stream?;
        let d = Arc::clone(&dir);
        let ci = Arc::clone(&circmgr);
        async_std::task::spawn(async move {
            let res = handle_socks_conn(d, ci, stream).await;
            if let Err(e) = res {
                warn!("connection edited with error: {}", e);
            }
        });
    }

    Ok(())
}

fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let filt = if args.trace {
        LevelFilter::Trace
    } else {
        LevelFilter::Debug
    };
    simple_logging::log_to_stderr(filt);

    if args.chutney_dir.is_none() && (args.flood || args.dl) {
        eprintln!("--flood and --dl both require --chutney-dir.");
        return Ok(());
    }

    let dir = get_netdir(&args)?;
    // TODO CONFORMANCE: we should stop now if there are required
    // protovers we don't support.

    async_std::task::block_on(async {
        let spawn = Spawner::new("channel reactors");
        let transport = NativeTlsTransport::new();
        let chanmgr = Arc::new(tor_chanmgr::ChanMgr::new(transport, spawn));

        let spawn = Spawner::new("circuit reactors");
        let circmgr = tor_circmgr::CircMgr::new(Arc::clone(&chanmgr), Box::new(spawn));

        if args.socksport.is_some() {
            return run_socks_proxy(dir, circmgr, args).await;
        }

        let exit_ports = &[80];
        let circ = circmgr.get_or_launch_exit(&dir, exit_ports).await?;

        info!("Built a three-hop circuit.");

        for _ in 0..args.n {
            if args.flood {
                test_cat(circ.new_ref()).await?;
            } else if args.dl {
                test_dl(circ.new_ref()).await?;
            } else {
                test_http(circ.new_ref()).await?;
            }
        }

        circ.terminate().await;

        async_std::task::sleep(std::time::Duration::new(3, 0)).await;
        Ok(())
    })
}
