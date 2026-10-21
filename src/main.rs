use std::{
    env,
    error::Error,
    io::{self, Write},
    process::ExitCode,
};

use buf_review::{
    diff, github,
    model::{Batch, BatchId, Review, ReviewId},
    store::Store,
    workspace_cli,
};
use clap::{Parser, Subcommand};
use serde::Serialize;
use url::Url;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "pr",
    version,
    about = "Review pull requests through local state"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute one structured workspace request from stdin; print JSON.
    Workspace,
    /// Fetch a GitHub PR; print the matching or newly stored review ID.
    Fetch { url: Url },
    /// List local review IDs as JSON.
    Reviews,
    /// Print a fetched review snapshot as JSON.
    Show { review_id: String },
    /// Create or inspect explicitly selected batches.
    Batch {
        #[command(subcommand)]
        command: BatchCommand,
    },
    /// Project a batch's event log at checked-out Git HEAD.
    State { batch_id: String },
    /// Print pending changes at Git HEAD, accounting for successful operations.
    Plan { batch_id: String },
    /// Print a diff between local commits; defaults to fetched base/head.
    Diff {
        review_id: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        head: Option<String>,
        #[arg(long)]
        path: Option<String>,
    },
}

#[derive(Subcommand)]
enum BatchCommand {
    /// Create an empty batch linked to a review; print its ID.
    New { review_id: String },
    /// List a review's batch IDs as JSON.
    List { review_id: String },
    /// Print a batch, including its complete ordered event log, as JSON.
    Open { batch_id: String },
}

fn json(value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn review(store: &Store, id: String) -> Result<Review, Box<dyn Error>> {
    store.get_review(&ReviewId(id.clone()))?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("review does not exist: {id}"),
        )
        .into()
    })
}

fn batch(store: &mut Store, id: String) -> Result<Batch, Box<dyn Error>> {
    store.get_batch(&BatchId(id.clone()))?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("batch does not exist: {id}"),
        )
        .into()
    })
}

fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let project = env::current_dir()?;
    if matches!(&cli.command, Commands::Workspace) {
        let request = serde_json::from_reader(io::stdin().lock())?;
        return json(&workspace_cli::run(&project, request)?);
    }
    if let Commands::Fetch { url } = cli.command {
        let fetched = github::fetch(&url)?;
        let id = Store::open(project)?.cache_import_review(
            &fetched.reviewer,
            &fetched.read,
            &fetched.backend,
            fetched.branches.as_ref(),
        )?;
        println!("{}", id.0);
        return Ok(());
    }
    let mut store = Store::open(&project)?;
    match cli.command {
        Commands::Workspace => unreachable!(),
        Commands::Fetch { .. } => unreachable!(),
        Commands::Reviews => json(&store.review_ids()?),
        Commands::Show { review_id } => json(&review(&store, review_id)?),
        Commands::Batch { command } => match command {
            BatchCommand::New { review_id } => {
                let review = review(&store, review_id)?;
                let id = BatchId(Uuid::new_v4().to_string());
                store.create_batch(&id, &review.id)?;
                println!("{}", id.0);
                Ok(())
            }
            BatchCommand::List { review_id } => {
                let review = review(&store, review_id)?;
                json(&store.batch_ids(&review.id)?)
            }
            BatchCommand::Open { batch_id } => json(&batch(&mut store, batch_id)?),
        },
        Commands::State { batch_id } => {
            let workspace = store.prepare_worktree(&batch_id)?;
            json(&workspace.projected()?)
        }
        Commands::Plan { batch_id } => {
            let workspace = store.prepare_worktree(&batch_id)?;
            json(&workspace.pending()?)
        }
        Commands::Diff {
            review_id,
            base,
            head,
            path,
        } => {
            let mut revision = review(&store, review_id)?.read.pull_request.revision;
            if let Some(base) = base {
                revision.base = base;
            }
            if let Some(head) = head {
                revision.head = head;
            }
            io::stdout()
                .lock()
                .write_all(&diff::diff(&project, &revision, path.as_deref())?)?;
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
