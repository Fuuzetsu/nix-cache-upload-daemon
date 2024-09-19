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
    // Future that should resolve when we want to terminate.
    stop: impl Future<Output = ()>,
) where
    L: Future<Output = ()> + Send + 'static,
    S: Future<Output = Option<Vec<String>>> + Send + 'static,
    U: Future<Output = ()> + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Signals when listener was done exactly once.
    enum Listener<F: Future<Output = ()>> {
        Running(std::pin::Pin<Box<F>>),
        Done,
    }

    impl<F> Future for Listener<F>
    where
        F: Future<Output = ()>,
    {
        type Output = ();

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            // tracing::info!("Polling listener");
            let this = self.get_mut();
            match this {
                Listener::Running(handle) => {
                    // Poll the JoinHandle
                    match std::pin::Pin::new(handle).poll(cx) {
                        std::task::Poll::Ready(_) => {
                            // Task completed, transition to Done state
                            *this = Listener::Done;
                            std::task::Poll::Ready(())
                        }
                        std::task::Poll::Pending => {
                            // tracing::info!("PENDING");
                            std::task::Poll::Pending
                        }
                    }
                }
                Listener::Done => std::task::Poll::Pending,
            }
        }
    }

    // Register line consumer.
    let listener = Listener::Running(Box::pin(listener(tx)));

    let signers = Arc::new(Semaphore::new(num_signers));
    let uploaders = Arc::new(Semaphore::new(num_uploaders));
    // Futures that we expect to exit before we're done.
    let running_jobs = Arc::new(Mutex::new(JoinSet::new()));

    // Consume messages from listener, spawn signers then uploaders.

    tokio::pin!(stop);
    tokio::pin!(listener);
    loop {
        let input = tokio::select! {
            // Signalled to stop.
            _ = &mut stop => {
                tracing::warn!("Got signalled to stop");
                break;
            }
            // Drops the channel if upstream is done. Unless it was cloned.
            // Either way, we gave it a chance.
            _ = &mut listener => {
                tracing::debug!("Input listener finished");
                continue;
            }
            input = rx.recv() => {
                match input {
                    Some(input) => input,
                    None => {
                        tracing::debug!("Channel closed");
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

        struct NoExplicitStop;

        // Never stop explicitly, we'll wait for the listener to close the channel.
        impl std::future::Future for NoExplicitStop {
            type Output = ();
            fn poll(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                std::task::Poll::Pending
            }
        }

        super::run(
            num_signers,
            num_uploaders,
            listener,
            signer,
            uploader,
            NoExplicitStop,
        )
        .await
    }
}
