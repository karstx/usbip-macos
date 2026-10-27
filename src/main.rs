use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use simple_logger::SimpleLogger;
use tokio::{net::TcpStream, runtime::Runtime};
use usbip_macos::{
    cli::{self, Action, Selector},
    client,
};

#[cfg(target_os = "macos")]
mod gui;

fn main() -> ExitCode {
    init_logger();
    #[cfg(target_os = "macos")]
    if launched_as_app() {
        return gui::run();
    }
    let args = cli::parse(&cli::command().get_matches());
    let runtime = match runtime() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.action {
        Action::List => runtime.block_on(list(&args.address)),
        Action::Attach {
            selector,
            wait_for_enter,
        } => attach(&runtime, &args.address, &selector, wait_for_enter),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn runtime() -> std::io::Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Opened from Finder or `open`, the bundle's executable gets no arguments; from a shell it
/// stays a CLI.
#[cfg(target_os = "macos")]
fn launched_as_app() -> bool {
    std::env::args_os().len() == 1
        && std::env::current_exe().is_ok_and(|exe| {
            exe.parent()
                .is_some_and(|dir| dir.ends_with("Contents/MacOS"))
        })
}

fn init_logger() {
    let default_level = if cfg!(debug_assertions) {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Warn
    };
    let _ = SimpleLogger::new()
        .with_utc_timestamps()
        .with_level(default_level)
        .env()
        .init();
}

async fn connect(address: &str) -> Result<TcpStream> {
    client::connect(address)
        .await
        .with_context(|| format!("connecting to {address}"))
}

async fn list(address: &str) -> Result<()> {
    let devices = client::list_devices(&mut connect(address).await?).await?;
    for d in devices {
        println!(
            "{}: {:04x}:{:04x} (bus {} dev {}, {:?} speed) {}",
            d.busid, d.id_vendor, d.id_product, d.busnum, d.devnum, d.speed, d.path
        );
        for (i, iface) in d.interfaces.iter().enumerate() {
            println!(
                "    {i}: class {:02x} subclass {:02x} protocol {:02x}",
                iface.class, iface.subclass, iface.protocol
            );
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn attach(
    _runtime: &Runtime,
    _address: &str,
    _selector: &Selector,
    _wait_for_enter: bool,
) -> Result<()> {
    Err(anyhow!("attaching devices is only supported on macOS"))
}

#[cfg(target_os = "macos")]
fn attach(
    runtime: &Runtime,
    address: &str,
    selector: &Selector,
    wait_for_enter: bool,
) -> Result<()> {
    use std::io::Write;
    use tokio::signal::unix::{signal, SignalKind};
    use usbip_macos::attach::Attachment;

    let attachment = runtime.block_on(async {
        let devices = client::list_devices(&mut connect(address).await?).await?;
        let device = selector
            .find(&devices)
            .ok_or_else(|| anyhow!("no exported device matches {selector:?}"))?;
        // The server closes the connection after a device list, so importing needs a new one.
        let attachment = Attachment::import(address, &device.busid).await?;
        log::info!("imported {:?}", attachment.session().device());
        anyhow::Ok(attachment)
    })?;

    if wait_for_enter {
        print!("Press Enter to connect the device...");
        std::io::stdout().flush()?;
        std::io::stdin().read_line(&mut String::new())?;
    }
    attachment.connect();

    let outcome = runtime.block_on(async {
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => Ok(()),
            _ = term.recv() => Ok(()),
            reason = attachment.closed() => Err(anyhow!("connection lost: {reason}")),
        }
    });

    log::info!("detaching");
    runtime.block_on(attachment.detach());
    outcome
}
