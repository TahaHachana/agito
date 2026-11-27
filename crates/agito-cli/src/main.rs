//! The `agito` command-line tool.

mod capture;
mod install;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;

use agito::BrowserChannel;
use agito::launch::finder;
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agito",
    version,
    about = "Browser automation and testing for Rust"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a browser for agito to launch.
    Install(InstallArgs),
    /// Save a screenshot of a page.
    Screenshot(capture::ScreenshotArgs),
    /// Save a page as a PDF.
    Pdf(capture::PdfArgs),
    /// Build and run the tests, with sharding, retries, quarantine and reports.
    Run(run::RunArgs),
    /// Work with the reports of a sharded run.
    Shard {
        #[command(subcommand)]
        command: ShardCommand,
    },
}

#[derive(Subcommand)]
enum ShardCommand {
    /// Merge the shards' JUnit reports into one, checking that every shard is there.
    Merge(run::merge::MergeArgs),
}

#[derive(Args)]
struct InstallArgs {
    /// The browser to install. Only chromium is downloaded; agito finds Chrome and Edge where
    /// their installers put them.
    #[arg(long, default_value = "chromium")]
    channel: BrowserChannel,

    /// The build: a version (131.0.6778.85), a milestone (131), a Chromium revision (1368529),
    /// or stable, beta, dev or canary.
    #[arg(long, default_value = "stable")]
    revision: install::Requested,

    /// The browser cache to install into. The launcher reads ~/.agito/browsers; a browser
    /// installed anywhere else is launched through AGITO_EXECUTABLE_PATH.
    #[arg(long)]
    path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Command::Install(args) => run_install(args).await,
        Command::Screenshot(args) => capture::screenshot(args).await,
        Command::Pdf(args) => capture::pdf(args).await,
        Command::Shard {
            command: ShardCommand::Merge(args),
        } => match run::merge::merge(args) {
            Ok(true) => Ok(()),
            Ok(false) => return ExitCode::FAILURE,
            Err(e) => Err(e),
        },
        Command::Run(args) => {
            // The run is blocking work: building, then running binaries one after another.
            match tokio::task::spawn_blocking(move || run::run(args)).await {
                Ok(Ok(true)) => Ok(()),
                Ok(Ok(false)) => return ExitCode::FAILURE,
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.into()),
            }
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_install(args: InstallArgs) -> Result<()> {
    match args.channel {
        BrowserChannel::Chromium => {}
        BrowserChannel::Chrome | BrowserChannel::Edge => bail!(
            "agito install downloads Chromium only. Install {} with its own installer; agito \
             finds it in the standard location, or through AGITO_EXECUTABLE_PATH.",
            if args.channel == BrowserChannel::Chrome {
                "Chrome"
            } else {
                "Edge"
            }
        ),
        BrowserChannel::Firefox => {
            bail!("agito does not drive Firefox yet, so it does not install it either")
        }
    }

    let default_root = finder::default_install_root();
    let root = args.path.unwrap_or_else(|| default_root.clone());
    let installed = install::install(&install::InstallOptions {
        root: root.clone(),
        requested: args.revision,
        platform: install::Platform::current()?,
        source: install::Source::default(),
    })
    .await?;

    println!(
        "Chromium {} installed at {}",
        installed.version,
        installed.executable.display()
    );
    if root != default_root {
        println!(
            "The launcher looks in {}; to launch this build, set AGITO_EXECUTABLE_PATH={}",
            default_root.display(),
            installed.executable.display()
        );
    }
    Ok(())
}
