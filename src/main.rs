use color_eyre::{eyre::WrapErr, Result};
use flexi_logger::{colored_detailed_format, LogTarget, Logger};
use notify_push::config::{Config, Opt};
use notify_push::message::DEBOUNCE_ENABLE;
use notify_push::metrics::serve_metrics;
use notify_push::{listen_loop, serve, App};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::oneshot;
use tokio::task::spawn;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let _ = dotenv::dotenv();

    let opt: Opt = Opt::from_args();
    if opt.version {
        println!("notify_push {}", env!("NOTIFY_PUSH_VERSION"));
        return Ok(());
    }
    let dump_config = opt.dump_config;
    let config = Config::from_opt(opt).wrap_err("Failed to parse config")?;

    if dump_config {
        println!("{:#?}", config);
        return Ok(());
    }

    let log_handle = Logger::with_str(&config.log_level)
        .log_target(LogTarget::StdOut)
        .format(colored_detailed_format)
        .start()?;

    let (serve_cancel, serve_cancel_handle) = oneshot::channel();
    let (metrics_cancel, metrics_cancel_handle) = oneshot::channel();
    let (listen_cancel, listen_cancel_handle) = oneshot::channel();

    log::trace!("Running with config: {:?}", config);

    if config.allow_self_signed {
        log::info!("Running with certificate validation disabled");
    }

    if dotenv::var("DEBOUNCE_DISABLE").is_ok() {
        DEBOUNCE_ENABLE.store(false, Ordering::Relaxed);
    }

    let bind = config.bind.clone();
    let metrics_bind = config.metrics_bind.clone();
    let app = Arc::new(App::new(config, log_handle).await?);
    if let Err(e) = app.self_test().await {
        log::error!("Self test failed: {:#}", e);
    }

    log::trace!("Listening on {}", bind);
    let server = spawn(serve(app.clone(), bind, serve_cancel_handle));

    if let Some(metrics_bind) = metrics_bind {
        log::trace!("Metrics listening {}", metrics_bind);
        spawn(serve_metrics(metrics_bind, metrics_cancel_handle));
    }

    spawn(listen_loop(app, listen_cancel_handle));

    // wait for either a sigint or sigterm
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;

    select! {
        _ = term.recv() => (),
        _ = int.recv() => (),
    };

    // then send cancel events to all of our spawned tasks

    log::info!("shutdown signal received, shutting down");

    serve_cancel.send(()).ok();
    metrics_cancel.send(()).ok();
    listen_cancel.send(()).ok();

    server.await?;

    Ok(())
}
