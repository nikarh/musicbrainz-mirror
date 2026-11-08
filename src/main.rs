//! The `musicbrainz-mirror` binary: serve (and keep up to date), import, healthcheck.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use musicbrainz_mirror::config::{Config, HELP, ImportArgs};
use musicbrainz_mirror::mirror::Mirror;
use musicbrainz_mirror::{store, ws};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("MUSICBRAINZ_MIRROR_VERSION");

fn main() -> ExitCode {
    let mut args = pico_args::Arguments::from_env();
    if args.contains(["-h", "--help"]) {
        let _ = writeln!(std::io::stdout(), "{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.contains(["-V", "--version"]) {
        let _ = writeln!(std::io::stdout(), "musicbrainz-mirror {VERSION}");
        return ExitCode::SUCCESS;
    }
    let command = match args.subcommand() {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let init_logging = || {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .init();
    };
    match command.as_deref() {
        Some("import") => {
            let args = match ImportArgs::parse(args) {
                Ok(a) => a,
                Err(e) => return fail(&e),
            };
            init_logging();
            match store::build::build(&args.sources, &args.dump, &args.out, &args.build) {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Some("healthcheck") => match Config::parse(&mut args) {
            Ok(config) => healthcheck(config.listen),
            Err(e) => fail(&e),
        },
        None | Some("serve") => {
            let config = match Config::parse(&mut args) {
                Ok(c) => c,
                Err(e) => return fail(&e),
            };
            init_logging();
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => return fail(&e),
            };
            runtime.block_on(serve(config))
        }
        Some(other) => fail(&format!("unknown command {other:?}; see --help")),
    }
}

fn fail(e: &dyn std::fmt::Display) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "musicbrainz-mirror: {e}");
    ExitCode::FAILURE
}

async fn serve(config: Config) -> ExitCode {
    // reqwest is built without a default crypto provider: ring builds much faster than aws-lc.
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing::info!(version = VERSION, "starting");
    tracing::info!(
        data_dir = %config.data_dir.display(),
        update_interval = %config
            .update_interval
            .map_or_else(|| "off".to_owned(), musicbrainz_mirror::mirror::human),
        min_free_gb = config.min_free_bytes >> 30,
        build_threads = config.build.threads,
        build_memory_mb = config.build.memory_mb,
        compress_rows = config.build.compress_rows,
        "settings"
    );
    let listen = config.listen;
    let cover_art_url = config.cover_art_url.clone();
    let mirror = match Mirror::new(config) {
        Ok(m) => Arc::new(m),
        Err(e) => return fail(&e),
    };
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => return fail(&format!("listening on {listen}: {e}")),
    };
    tracing::info!(%listen, "listening");

    let stop = tokio_util::sync::CancellationToken::new();
    let failed = Arc::new(AtomicBool::new(false));
    let background = tokio::spawn({
        let (mirror, stop, failed) = (Arc::clone(&mirror), stop.clone(), Arc::clone(&failed));
        async move {
            if let Err(e) = mirror.start().await {
                tracing::error!(error = %e, "can't start");
                failed.store(true, Ordering::Relaxed);
                stop.cancel();
                return;
            }
            mirror.run_updates().await;
        }
    });

    let app = ws::router(mirror.current(), &cover_art_url);
    let shutdown = {
        let stop = stop.clone();
        async move {
            tokio::select! {
                () = signal() => tracing::info!("stopping"),
                () = stop.cancelled() => {}
            }
        }
    };
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;
    background.abort();
    match served {
        Err(e) => fail(&e),
        Ok(()) if failed.load(Ordering::Relaxed) => ExitCode::FAILURE,
        Ok(()) => ExitCode::SUCCESS,
    }
}

async fn signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "no SIGTERM handler");
            let _ = ctrl_c.await;
            return;
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}

/// For the image's HEALTHCHECK (a scratch image has no curl): GET /healthz on the configured port.
fn healthcheck(listen: SocketAddr) -> ExitCode {
    let addr = SocketAddr::from(([127, 0, 0, 1], listen.port()));
    let check = || -> std::io::Result<bool> {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
    };
    match check() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => fail(&"healthcheck: not healthy"),
        Err(e) => fail(&format!("healthcheck: {e}")),
    }
}
