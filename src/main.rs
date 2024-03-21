use bpaf::*;
use std::io::Read;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

struct Opts {
    num_signers: usize,
    num_uploaders: usize,
    thread_stack_size: usize,
    private_key_path: std::ffi::OsString,
    cache_uri: std::ffi::OsString,
    listen: std::net::SocketAddr,
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
        let thread_stack_size = ::bpaf::long("thread-stack-size")
            .help("Stack size for spawned threads. 512KiB by default.")
            .argument::<usize>("STACK_SIZE")
            .fallback(512 * 1024);
        let private_key_path = ::bpaf::long("private-key-path")
            .help("Path to the signing private key")
            .argument::<std::ffi::OsString>("PATH");
        let cache_uri = ::bpaf::long("cache-uri")
            .help("Cache URI in format nix copy expects it in")
            .argument::<std::ffi::OsString>("CACHE");
        let listen = ::bpaf::long("listen")
            .help("Address to listen on")
            .argument::<std::net::SocketAddr>("listen");
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
            thread_stack_size,
            private_key_path,
            cache_uri,
            listen,
            pid_file,
            stdout_file,
            stderr_file,
            tracing_file
        })
    }
}

fn main() {
    let Opts {
        num_signers,
        num_uploaders,
        thread_stack_size,
        private_key_path,
        cache_uri,
        listen,
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

    let listener = std::net::TcpListener::bind(listen).unwrap();
    tracing::debug!(?listen, "Listening for requests");

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

    let (worker_tx, worker_rx) = crossbeam_channel::unbounded();

    ctrlc::set_handler({
        let worker_tx = worker_tx.clone();
        move || {
            tracing::debug!("Termination signal received...");
            let _ = worker_tx.send(Msg::Stop);
        }
    })
    .unwrap();

    let worker = std::thread::spawn(move || {
        tracing::debug!(
            ?num_signers,
            ?thread_stack_size,
            "Spawning signer threadpool"
        );
        let signers = threadpool::Builder::new()
            .num_threads(num_signers)
            .thread_stack_size(thread_stack_size)
            .build();

        tracing::debug!(
            ?num_uploaders,
            ?thread_stack_size,
            "Spawning uploader threadpool",
        );
        let uploaders = std::sync::Arc::new(std::sync::Mutex::new(
            threadpool::Builder::new()
                .num_threads(num_uploaders)
                .thread_stack_size(thread_stack_size)
                .build(),
        ));
        let private_key_path = std::sync::Arc::new(private_key_path);
        let cache_uri = std::sync::Arc::new(cache_uri);
        let mut stop = false;
        loop {
            match worker_rx.recv() {
                Ok(msg) => match msg {
                    Msg::Stop => {
                        tracing::debug!("Worker was asked to terminate...");
                        stop = true;
                    }
                    Msg::Input(line) => {
                        tracing::debug!("Received work: {line}");
                        let uploaders = uploaders.clone();
                        let private_key_path = private_key_path.clone();
                        let cache_uri = cache_uri.clone();
                        signers.execute(move || {
                            let paths = line.split_whitespace().map(|s| s.to_owned()).collect::<Vec<_>>();
                            tracing::debug!(?paths, ?private_key_path, "Signing {} paths", paths.len());
                            match std::process::Command::new("nix")
                                .arg("store")
                                .arg("sign")
                                .arg("-k")
                                .arg(private_key_path.as_os_str())
                                .args(&paths)
                                .status()
                            {
                                Ok(status) => {
                                    if status.success() {
                                        tracing::debug!(?paths, ?cache_uri, "Uploading {} paths", paths.len());
                                        uploaders.lock().unwrap().execute(move || {
                                            match std::process::Command::new("nix").arg("copy").arg("--to").arg(cache_uri.as_os_str()).args(&paths).status() {
                                                Ok(status) => if status.success() {
                                                    tracing::info!(?paths, ?cache_uri, "Signed and uploaded {} paths", paths.len())
                                                } else {
                                                    tracing::error!(?paths, ?cache_uri, ?status, "Failed to upload {} paths", paths.len())
                                                },
                                                Err(error) => {
                                                    tracing::error!(
                                                        ?paths,
                                                        ?error,
                                                        ?cache_uri,
                                                        "Failed to execute upload process for {} paths",
                                                        paths.len()
                                                    )
                                                },
                                            }
                                        })
                                    } else {
                                        tracing::error!(
                                            ?paths,
                                            ?status,
                                            "Failed to sign some paths, skipping upload"
                                        );
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(
                                        ?paths,
                                        ?error,
                                        "Failed to execute signing process skipping upload of {} paths",
                                        paths.len()
                                    )
                                }
                            }
                        });
                    }
                },
                Err(error) => {
                    tracing::error!(?error, "Work channel closed, terminating work thread...");
                    stop = true;
                }
            }
            if stop {
                tracing::debug!("Waiting for signers to terminate");
                signers.join();
                tracing::debug!("Waiting for uploaders to terminate");
                uploaders.lock().unwrap().join();
                break;
            }
        }
    });

    let _line_consumer = std::thread::spawn(move || loop {
        if let Ok((mut stream, conn)) = listener.accept() {
            tracing::debug!(?conn, "New client");
            let worker_tx = worker_tx.clone();
            let _listen = std::thread::spawn(move || {
                let mut input = String::default();
                let _ = stream.read_to_string(&mut input);

                if input.is_empty() {
                    tracing::debug!(?conn, "No input from client, doing nothing");
                } else {
                    tracing::debug!(?input, ?conn, "Got input from client");
                    let _ = worker_tx.send(Msg::Input(std::mem::take(&mut input)));
                }
                drop(stream);
                tracing::debug!(?conn, "Disconnected");
            });
        }
    });

    // Wait for worker to finish on whatever lines it had so far. Expilictly do
    // _not_ wait for the line consumer as that might be blocking forever on
    // stdin.
    worker.join().unwrap();
}

enum Msg {
    Stop,
    Input(String),
}
