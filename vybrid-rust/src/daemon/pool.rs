use anyhow::Result;
use chrono::Utc;
use console::style;
use fs2::FileExt;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use uuid::Uuid;

use crate::config::Config;

use super::queue::{ExecutionRequest, MessageQueue};
use super::worker::Worker;

/// Daemon pool lock file data
#[derive(Debug, Serialize, Deserialize)]
struct DaemonLock {
    pid: u32,
    timestamp: String,
    session_id: String,
    workers: usize,
}

/// Start the daemon pool
pub async fn start_daemon_pool(config: Config) -> Result<()> {
    let max_workers = 3;
    let session_id = Uuid::new_v4().to_string();

    // Check if daemon is already running
    let lock_file = config.daemon_lock_file();
    if is_daemon_running(&lock_file)? {
        eprintln!("{}", style("Daemon pool is already running!").yellow());
        return Ok(());
    }

    // Create daemon lock
    create_daemon_lock(&lock_file, max_workers, &session_id)?;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Handle Ctrl+C
    ctrlc::set_handler(move || {
        eprintln!("\n{}", style("Stopping daemon pool...").yellow());
        r.store(false, Ordering::SeqCst);
    })?;

    eprintln!("{}", style("Daemon Pool Started").green().bold());
    eprintln!("Workers: {}", max_workers);
    eprintln!("Session: {}", &session_id[..8]);
    eprintln!("Messages dir: {}", config.messages_dir.display());
    eprintln!("{}", style("Press Ctrl+C to stop").dim());
    eprintln!();

    // Create message queue
    let queue = Arc::new(MessageQueue::new(
        config.messages_dir.clone(),
        config.progress_dir.clone(),
    ));

    // Create work channel
    let (tx, rx): (Sender<ExecutionRequest>, Receiver<ExecutionRequest>) = mpsc::channel();
    let rx = Arc::new(std::sync::Mutex::new(rx));

    // Start worker threads
    let mut worker_handles = Vec::new();
    for i in 0..max_workers {
        let config_clone = config.clone();
        let queue_clone = queue.clone();
        let running_clone = running.clone();
        let rx_clone = rx.clone();

        let handle = thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            let worker = Worker::new(i, &config_clone, queue_clone.clone(), running_clone.clone());

            while running_clone.load(Ordering::SeqCst) {
                // Try to get a request from the channel
                let request = {
                    let rx_guard = rx_clone.lock().unwrap();
                    rx_guard.recv_timeout(Duration::from_secs(1))
                };

                if let Ok(request) = request {
                    let response = rt.block_on(worker.process_request(request));
                    match response {
                        Ok(resp) => {
                            if let Err(e) = queue_clone.send_response(&resp) {
                                eprintln!("Failed to send response: {}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("Worker {} error: {}", i, e);
                        }
                    }
                }
            }

            eprintln!("{}", style(format!("Worker {} stopped", i)).dim());
        });

        worker_handles.push(handle);
    }

    eprintln!("{} {} workers started", style("✓").green(), max_workers);

    // Set up file watcher for new requests
    let tx_watcher = tx.clone();
    let messages_dir = config.messages_dir.clone();

    let (watcher_tx, watcher_rx) = mpsc::channel();
    let mut watcher: RecommendedWatcher = Watcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = watcher_tx.send(event);
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_millis(500)),
    )?;

    watcher.watch(&messages_dir, RecursiveMode::NonRecursive)?;

    eprintln!("{} File watcher started", style("✓").green());
    eprintln!("{}", style("Waiting for requests...").dim());

    // Main loop
    let mut cleanup_counter = 0;
    while running.load(Ordering::SeqCst) {
        // Check for file watcher events
        while let Ok(event) = watcher_rx.try_recv() {
            for path in event.paths {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("request_") && name.ends_with(".json") {
                        // Small delay to ensure file is fully written
                        thread::sleep(Duration::from_millis(100));

                        // Read and queue the request
                        if let Ok(content) = fs::read_to_string(&path) {
                            if let Ok(request) = serde_json::from_str::<ExecutionRequest>(&content) {
                                eprintln!(
                                    "{} New request detected: {}",
                                    style("→").cyan(),
                                    &request.id[..8]
                                );
                                let _ = tx_watcher.send(request);
                            }
                        }
                    }
                }
            }
        }

        // Also check for any missed requests (fallback)
        if let Ok(pending) = queue.get_pending_requests() {
            for request in pending {
                let _ = tx.send(request);
            }
        }

        // Periodic cleanup
        cleanup_counter += 1;
        if cleanup_counter >= 60 {
            // Every ~60 seconds
            cleanup_counter = 0;
            if let Err(e) = queue.cleanup_old_messages() {
                eprintln!("Cleanup error: {}", e);
            }
        }

        thread::sleep(Duration::from_secs(1));
    }

    // Wait for workers to finish
    eprintln!("{}", style("Waiting for workers to finish...").dim());
    for handle in worker_handles {
        let _ = handle.join();
    }

    // Remove daemon lock
    remove_daemon_lock(&lock_file)?;

    eprintln!("{}", style("Daemon Pool Stopped").yellow().bold());
    Ok(())
}

fn is_daemon_running(lock_file: &PathBuf) -> Result<bool> {
    if !lock_file.exists() {
        return Ok(false);
    }

    // Try to read and check if lock is stale
    let content = fs::read_to_string(lock_file)?;
    let lock: DaemonLock = serde_json::from_str(&content)?;

    // Check if timestamp is older than 10 minutes
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&lock.timestamp) {
        let age = Utc::now().signed_duration_since(timestamp);
        if age.num_minutes() > 10 {
            // Lock is stale, remove it
            fs::remove_file(lock_file)?;
            return Ok(false);
        }
    }

    // Check if process is still running
    let path = PathBuf::from(format!("/proc/{}", lock.pid));
    if !path.exists() {
        // Process is dead, remove lock
        fs::remove_file(lock_file)?;
        return Ok(false);
    }

    Ok(true)
}

fn create_daemon_lock(lock_file: &PathBuf, workers: usize, session_id: &str) -> Result<()> {
    let lock = DaemonLock {
        pid: std::process::id(),
        timestamp: Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        workers,
    };

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(lock_file)?;

    file.lock_exclusive()?;
    let json = serde_json::to_string_pretty(&lock)?;
    file.write_all(json.as_bytes())?;
    file.unlock()?;

    Ok(())
}

fn remove_daemon_lock(lock_file: &PathBuf) -> Result<()> {
    if lock_file.exists() {
        fs::remove_file(lock_file)?;
    }
    Ok(())
}
