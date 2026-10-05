//! Background tasks feeding the TUI: daemon status, log stream, Clash API.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;

use super::app::AppEvent;
use crate::clash::ClashClient;
use crate::client::DaemonClient;
use crate::protocol::ClashApi;
use crate::substore::SubStoreClient;

pub type EventTx = mpsc::UnboundedSender<AppEvent>;

pub fn spawn_all(
    client: DaemonClient,
    tx: EventTx,
    clash_api: watch::Receiver<Option<ClashApi>>,
    refresh: Arc<Notify>,
    sub_store_api: watch::Receiver<Option<String>>,
    store_refresh: Arc<Notify>,
) -> JoinSet<()> {
    let mut set = JoinSet::new();
    set.spawn(poll_status(client.clone(), tx.clone()));
    set.spawn(follow_logs(client, tx.clone()));
    set.spawn(poll_clash(tx.clone(), clash_api, refresh));
    set.spawn(poll_sub_store(tx, sub_store_api, store_refresh));
    set
}

/// Lists Sub-Store subscriptions every 5s while Sub-Store is running.
async fn poll_sub_store(
    tx: EventTx,
    mut api: watch::Receiver<Option<String>>,
    refresh: Arc<Notify>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let mut current: Option<(String, SubStoreClient)> = None;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = refresh.notified() => {}
            changed = api.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        let wanted = api.borrow().clone();
        let Some(wanted) = wanted else {
            current = None;
            continue;
        };
        if current.as_ref().is_none_or(|(have, _)| *have != wanted) {
            match SubStoreClient::new(&wanted) {
                Ok(client) => current = Some((wanted, client)),
                Err(err) => {
                    let _ = tx.send(AppEvent::SubStore(Err(format!("{err:#}"))));
                    continue;
                }
            }
        }
        let Some((_, client)) = &current else {
            continue;
        };
        let result = client.overview().await.map_err(|err| format!("{err:#}"));
        if tx.send(AppEvent::SubStore(result)).is_err() {
            return;
        }
    }
}

async fn poll_status(client: DaemonClient, tx: EventTx) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let result = client.status().await.map_err(|err| format!("{err:#}"));
        if tx.send(AppEvent::Status(result)).is_err() {
            return;
        }
    }
}

async fn follow_logs(client: DaemonClient, tx: EventTx) {
    loop {
        if let Ok(mut stream) = client.logs(1000, true).await {
            // The backlog is replayed on every (re)connect.
            if tx.send(AppEvent::LogsReset).is_err() {
                return;
            }
            while let Ok(Some(entry)) = stream.next().await {
                if tx.send(AppEvent::Log(entry)).is_err() {
                    return;
                }
            }
        }
        if tx.send(AppEvent::LogsDisconnected).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Polls `/connections` every second, `/proxies` every 3s and `/configs`
/// every 5s; `refresh` forces an immediate full poll after user actions.
async fn poll_clash(tx: EventTx, mut api: watch::Receiver<Option<ClashApi>>, refresh: Arc<Notify>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut current: Option<(ClashApi, ClashClient)> = None;
    let mut tick: u64 = 0;
    loop {
        let mut full = false;
        tokio::select! {
            _ = interval.tick() => {}
            _ = refresh.notified() => full = true,
            changed = api.changed() => {
                if changed.is_err() {
                    return;
                }
                full = true;
            }
        }
        let wanted = api.borrow().clone();
        let Some(wanted) = wanted else {
            current = None;
            continue;
        };
        if current.as_ref().is_none_or(|(have, _)| *have != wanted) {
            match ClashClient::new(&wanted) {
                Ok(client) => {
                    current = Some((wanted, client));
                    full = true;
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::ClashError(format!("{err:#}")));
                    continue;
                }
            }
        }
        let Some((_, clash)) = &current else { continue };
        tick += 1;

        let result = clash.connections().await;
        let ok = result.is_ok();
        let event = match result {
            Ok(connections) => AppEvent::Connections(connections),
            Err(err) => AppEvent::ClashError(format!("{err:#}")),
        };
        if tx.send(event).is_err() {
            return;
        }
        if !ok {
            continue;
        }
        if (full || tick.is_multiple_of(3))
            && let Ok(proxies) = clash.proxies().await
        {
            let _ = tx.send(AppEvent::Proxies(proxies));
        }
        if (full || tick.is_multiple_of(5))
            && let Ok(configs) = clash.configs().await
        {
            let _ = tx.send(AppEvent::Configs(configs));
        }
    }
}
