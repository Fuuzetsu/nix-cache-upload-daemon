use std::{future::Future, sync::Arc};
use tokio::{
    sync::{mpsc::UnboundedSender, Mutex, Semaphore},
    task::JoinSet,
};

pub async fn run<L, S, U>(
    num_signers: usize,
    num_uploaders: usize,
    listener: impl FnOnce(UnboundedSender<String>) -> L,
    signer: impl Fn(String) -> S + Send + Clone + 'static,
    uploader: impl Fn(Vec<String>) -> U + Send + Clone + 'static,
) where
    L: Future<Output = ()> + Send + 'static,
    S: Future<Output = Option<Vec<String>>> + Send + 'static,
    U: Future<Output = ()> + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    let signers = Arc::new(Semaphore::new(num_signers));
    let uploaders = Arc::new(Semaphore::new(num_uploaders));
    // Futures that we expect to exit before we're done.
    let running_jobs = Arc::new(Mutex::new(JoinSet::new()));

    // Register line consumer.
    let listener = listener(tx);

    tokio::pin!(listener);
    let mut listener_done = false;
    loop {
        let input = tokio::select! {
            // Drops the channel if upstream is done. Unless it was cloned.
            // Either way, we gave it a chance.
            _ = &mut listener, if !listener_done => {
                listener_done = true;
                tracing::debug!("Input listener finished");
                continue;
            }
            input = rx.recv() => {
                match input {
                    Some(input) => input,
                    None => {
                        tracing::debug!("Input channel closed");
                        break;
                    }
                }
            }
        };

        running_jobs.lock().await.spawn({
            let signer = signer.clone();
            let uploader = uploader.clone();
            let signers = signers.clone();
            let uploaders = uploaders.clone();
            async move {
                let Some(paths) = async move {
                    let _permit = signers.acquire().await.ok()?;
                    // signer(input).await
                    signer(input).await
                }
                .await
                else {
                    return;
                };

                if let Ok(_permit) = uploaders.acquire().await {
                    uploader(paths).await
                }
            }
        });
    }

    // We're done with processing any new message here: we probably saw a
    // termination signal. Wait for any remaining uploaders and signers to
    // finish.
    tracing::debug!("Waiting for jobs to terminate");
    let mut running_jobs = running_jobs.lock().await;
    while running_jobs.join_next().await.is_some() {}
    tracing::debug!("All jobs terminated, done!");
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::UnboundedSender;

    #[test_log::test(tokio::test)]
    async fn mock() {
        let num_signers = 10;
        let num_uploaders = 2;

        let listener = |tx: UnboundedSender<String>| async move {
            tracing::info!("started listener");
            for ix in 0..10 {
                tracing::info!("sending: {ix}");
                tx.send(format!("input ix {ix}")).unwrap();
            }
        };
        let signer = |input: String| async move {
            tracing::info!("signer: {input}");
            Some(vec![input])
        };
        let uploader = |paths: Vec<String>| async move {
            tracing::info!("uploader: {paths:?}");
        };

        super::run(num_signers, num_uploaders, listener, signer, uploader).await
    }
}
