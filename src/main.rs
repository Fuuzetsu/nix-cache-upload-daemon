use bpaf::*;
use tokio::{io::AsyncReadExt, sync::mpsc::UnboundedSender};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

struct Opts {
    num_signers: usize,
    num_uploaders: usize,
    private_key_path: std::ffi::OsString,
    cache_uri: std::ffi::OsString,
    listen: std::net::SocketAddr,
    no_daemon: bool,
    pid_file: Option<std::path::PathBuf>,
    stdout_file: Option<std::path::PathBuf>,
    stderr_file: Option<std::path::PathBuf>,
    tracing_file: Option<std::path::PathBuf>,
}

fn opts() -> impl ::bpaf::Parser<Opts> {
    {
        let num_signers = ::bpaf::long("num-signers")
            .help("Number of threads to use to sign paths.")
            .argument::<usize>("NUM_SIGNERS")
            .fallback(4);
        let num_uploaders = ::bpaf::long("num-uploaders")
            .help("Number of threads to use to upload signed paths.")
            .argument::<usize>("NUM_UPLOADERS")
            .fallback(64);
        let private_key_path = ::bpaf::long("private-key-path")
            .help("Path to the signing private key")
            .argument::<std::ffi::OsString>("PATH");
        let cache_uri = ::bpaf::long("cache-uri")
            .help("Cache URI in format nix copy expects it in")
            .argument::<std::ffi::OsString>("CACHE");
        let listen = ::bpaf::long("listen")
            .help("Address to listen on")
            .argument::<std::net::SocketAddr>("listen");
        let no_daemon = ::bpaf::long("no-daemon").switch().help("Do not daemonise");
        let pid_file = ::bpaf::long("pid-file")
            .help("PID file to write to after forking")
            .argument::<std::path::PathBuf>("PID_FILE")
            .optional();
        let stdout_file = ::bpaf::long("stdout-file")
            .help("File to write stdout to after forking")
            .argument::<std::path::PathBuf>("FILE")
            .optional();
        let stderr_file = ::bpaf::long("stderr-file")
            .help("File to write stderr to after forking")
            .argument::<std::path::PathBuf>("FILE")
            .optional();
        let tracing_file = ::bpaf::long("tracing-file")
            .help("File to write tracing logs to")
            .argument::<std::path::PathBuf>("FILE")
            .optional();

        ::bpaf::construct!(Opts {
            num_signers,
            num_uploaders,
            private_key_path,
            cache_uri,
            listen,
            no_daemon,
            pid_file,
            stdout_file,
            stderr_file,
            tracing_file
        })
    }
}

#[tokio::main]
async fn main() {
    let Opts {
        num_signers,
        num_uploaders,
        private_key_path,
        cache_uri,
        listen,
        no_daemon,
        pid_file,
        stdout_file,
        stderr_file,
        tracing_file,
    } = opts().run();

    let subs = tracing_subscriber::registry().with(EnvFilter::from_default_env());

    if let Some(tracing_file) = tracing_file {
        let tracing_file = std::fs::File::create(tracing_file).unwrap();
        subs.with(fmt::layer().with_writer(tracing_file)).init();
    } else {
        subs.with(fmt::layer()).init();
    };

    let listener = tokio::net::TcpListener::bind(listen).await.unwrap();
    tracing::debug!(?listen, "Listening for requests");

    if !no_daemon {
        let mut daemon = daemonize::Daemonize::new();
        if let Some(pid_file) = pid_file {
            daemon = daemon.pid_file(pid_file);
        }
        if let Some(stdout_file) = stdout_file {
            let stdout_file = std::fs::File::create(stdout_file).unwrap();
            daemon = daemon.stdout(stdout_file);
        }
        if let Some(stderr_file) = stderr_file {
            let stderr_file = std::fs::File::create(stderr_file).unwrap();
            daemon = daemon.stderr(stderr_file);
        }

        daemon.start().unwrap();
    }

    // Spawn early so we handle Ctrl-C from here...
    //
    // I think we have a race between daemonisation and registering the signal.
    // Do we need to register the signal earlier?
    //
    // We need to poll the future straight away (which registers the signal) but
    // we want to yield only later, after all the other stuff is started. So we
    // wrap in tokio::spawn which will register and we can poll it later to get
    // the result.
    let stop = tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            tracing::debug!("Termination signal received...");
        }
    });

    // Yield the stop, throwing away any failure (I/O?)
    let stop = async move {
        let _ = stop.await;
    };

    let listener = |tx: UnboundedSender<String>| async move {
        loop {
            let Ok((mut stream, conn)) = listener.accept().await else {
                continue;
            };
            tracing::debug!(?conn, "New client");
            // Spawn a future to handle the client so that we can accept more
            // connections. We never await these explicitly as the client can
            // just connect and sit there... We could limit the number of
            // concurrent futures but don't bother for now...
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut input = String::default();
                let _ = stream.read_to_string(&mut input).await;

                if input.is_empty() {
                    tracing::debug!(?conn, "No input from client, doing nothing");
                } else {
                    tracing::debug!(?input, ?conn, "Got input from client");
                    let _ = tx.send(std::mem::take(&mut input));
                }
                drop(stream);
                tracing::debug!(?conn, "Disconnected");
            });
        }
    };

    let uploader = move |paths: Vec<String>| {
        let cache_uri = cache_uri.clone();
        async move {
            match tokio::process::Command::new("nix")
                .arg("copy")
                .arg("--to")
                .arg(cache_uri.as_os_str())
                .args(&paths)
                .status()
                .await
            {
                Ok(status) => {
                    if status.success() {
                        tracing::info!(
                            ?paths,
                            ?cache_uri,
                            "Signed and uploaded {} paths",
                            paths.len()
                        )
                    } else {
                        tracing::error!(
                            ?paths,
                            ?cache_uri,
                            ?status,
                            "Failed to upload {} paths",
                            paths.len()
                        )
                    }
                }
                Err(error) => {
                    tracing::error!(
                        ?paths,
                        ?error,
                        ?cache_uri,
                        "Failed to execute upload process for {} paths",
                        paths.len()
                    )
                }
            }
        }
    };

    let signer = move |line: String| {
        let private_key_path = private_key_path.clone();
        async move {
            let paths = line
                .split_whitespace()
                .map(|s| s.to_owned())
                .collect::<Vec<_>>();
            tracing::debug!(?paths, ?private_key_path, "Signing {} paths", paths.len());
            match tokio::process::Command::new("nix")
                .arg("store")
                .arg("sign")
                .arg("-k")
                .arg(private_key_path.as_os_str())
                .args(&paths)
                .status()
                .await
            {
                Ok(status) => {
                    if status.success() {
                        tracing::debug!(?paths, "Signed {} paths", paths.len());
                        Some(paths)
                    } else {
                        tracing::error!(
                            ?paths,
                            ?status,
                            "Failed to sign some paths, skipping upload"
                        );
                        None
                    }
                }
                Err(error) => {
                    tracing::error!(
                        ?paths,
                        ?error,
                        "Failed to execute signing process skipping upload of {} paths",
                        paths.len()
                    );
                    None
                }
            }
        }
    };

    nix_cache_upload_daemon::run(num_signers, num_uploaders, listener, signer, uploader, stop).await
}
