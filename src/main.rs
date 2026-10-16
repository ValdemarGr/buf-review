use std::{
    env,
    error::Error,
    fs::File,
    io::{self, Write},
    process::ExitCode,
};

use buf_review::{
    core, diff, github,
    model::{Batch, BatchId, Event, Review, ReviewId},
    store::Store,
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
    /// Fetch a GitHub PR into a new local review snapshot; print its ID.
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
    /// Append one Event JSON from stdin; print the retained event as JSON.
    Stage { batch_id: String },
    /// Project a batch's complete event log over its fetched snapshot.
    State { batch_id: String },
    /// Print pending net changes after accounting for successful operations.
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
    /// Publish pending net changes; print successful operation receipts.
    Commit { batch_id: String },
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
    if let Commands::Fetch { url } = cli.command {
        let fetched = github::fetch(&url)?;
        let id = ReviewId(Uuid::new_v4().to_string());
        Store::open(project)?.import_review(
            &id,
            &fetched.reviewer,
            &fetched.read,
            &fetched.backend,
        )?;
        println!("{}", id.0);
        return Ok(());
    }
    let mut store = Store::open(&project)?;
    match cli.command {
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
        Commands::Stage { batch_id } => {
            let event: Event = serde_json::from_reader(io::stdin().lock())?;
            store.append_event(&BatchId(batch_id), &event)?;
            json(&event)
        }
        Commands::State { batch_id } => {
            let batch = batch(&mut store, batch_id)?;
            let review = review(&store, batch.review.0.clone())?;
            json(&core::project(&review, &batch)?)
        }
        Commands::Plan { batch_id } => {
            let batch = batch(&mut store, batch_id)?;
            let review = review(&store, batch.review.0.clone())?;
            let completed = store.operation_results(&batch.id)?;
            let operations = completed
                .into_iter()
                .map(|result| result.operation)
                .collect::<Vec<_>>();
            json(&core::prepare(&review, &batch, &operations)?)
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
        Commands::Commit { batch_id } => {
            let commit_lock = File::open(project.join(".review/state.sqlite3"))?;
            commit_lock.lock()?;
            let batch = batch(&mut store, batch_id)?;
            let review = review(&store, batch.review.0.clone())?;
            let completed = store.operation_results(&batch.id)?;
            let operations = completed
                .into_iter()
                .map(|result| result.operation)
                .collect::<Vec<_>>();
            let pending = core::prepare(&review, &batch, &operations)?;
            let mut backend = store.review_backend(&review.id)?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "review has no backend metadata")
            })?;
            if backend.source.backend != "github" {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unsupported backend: {}", backend.source.backend),
                )
                .into());
            }
            github::preflight(&review, &backend, &pending)?;
            for operation in &pending.operations {
                let result = github::publish(&review, &backend, operation).map_err(|error| {
                    io::Error::other(format!(
                        "operation {} failed: {error}; earlier successful operations remain recorded",
                        operation.id
                    ))
                })?;
                store.record_operation_result(
                    &batch.id,
                    operation,
                    &result.mappings,
                    &result.response,
                ).map_err(|error| {
                    io::Error::other(format!(
                        "operation {} succeeded remotely but saving its receipt failed: {error}; remote write may repeat on retry",
                        operation.id
                    ))
                })?;
                backend = store.review_backend(&review.id)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "review backend metadata disappeared",
                    )
                })?;
            }
            json(&store.operation_results(&batch.id)?)
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
