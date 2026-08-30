//! Linux BlueZ BLE pairing advertisement with a secure long-read bundle.

#[cfg(target_os = "linux")]
mod platform {
    use std::{
        io::Write,
        sync::mpsc,
        thread::{self, JoinHandle},
        time::Duration,
    };

    use bluer::{
        adv::{Advertisement, Type},
        agent::{Agent, AuthorizeService, ReqError, RequestAuthorization, RequestConfirmation},
        gatt::local::{Application, Characteristic, CharacteristicRead, Service},
    };
    use futures::FutureExt;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    /// Running Linux BLE advertisement and GATT service.
    pub struct BlePairingAdvertiser {
        stop: Option<oneshot::Sender<()>>,
        join: Option<JoinHandle<()>>,
    }

    impl BlePairingAdvertiser {
        /// Starts BlueZ advertisement, DisplayYesNo agent, and secure bundle read.
        pub fn start(pairing_uri: &str) -> Result<Self, String> {
            let pairing_uri = pairing_uri.as_bytes().to_vec();
            let (stop_tx, stop_rx) = oneshot::channel();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let join = thread::Builder::new()
                .name("agentpulse-ble-pairing".to_owned())
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .worker_threads(2)
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    runtime.block_on(run(pairing_uri, stop_rx, ready_tx));
                })
                .map_err(|error| error.to_string())?;
            match ready_rx.recv_timeout(Duration::from_secs(8)) {
                Ok(Ok(())) => Ok(Self {
                    stop: Some(stop_tx),
                    join: Some(join),
                }),
                Ok(Err(error)) => {
                    let _ = join.join();
                    Err(error)
                }
                Err(_) => {
                    let _ = stop_tx.send(());
                    let _ = join.join();
                    Err("BlueZ did not become ready before the deadline".to_owned())
                }
            }
        }
    }

    impl Drop for BlePairingAdvertiser {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    async fn run(
        pairing_uri: Vec<u8>,
        stop: oneshot::Receiver<()>,
        ready: mpsc::SyncSender<Result<(), String>>,
    ) {
        let result = prepare(pairing_uri).await;
        let (handles, readiness) = match result {
            Ok(handles) => (Some(handles), Ok(())),
            Err(error) => (None, Err(error.to_string())),
        };
        let _ = ready.send(readiness);
        if let Some(handles) = handles {
            let _ = stop.await;
            drop(handles);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn prepare(pairing_uri: Vec<u8>) -> bluer::Result<BleHandles> {
        let service_uuid = Uuid::from_u128(0xd22e50f9015e53babe493e4d235f3288);
        let characteristic_uuid = Uuid::from_u128(0xea63bfc987c35074aa3749b6a617569b);
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;
        let authorize_uuid = service_uuid;
        let agent = Agent {
            request_default: true,
            request_confirmation: Some(Box::new(|request: RequestConfirmation| {
                async move {
                    let approved = tokio::task::spawn_blocking(move || {
                        confirm_numeric(request.device.to_string(), request.passkey)
                    })
                    .await
                    .unwrap_or(false);
                    if approved {
                        Ok(())
                    } else {
                        Err(ReqError::Rejected)
                    }
                }
                .boxed()
            })),
            request_authorization: Some(Box::new(|request: RequestAuthorization| {
                async move {
                    let approved = tokio::task::spawn_blocking(move || {
                        confirm_authorization(request.device.to_string())
                    })
                    .await
                    .unwrap_or(false);
                    if approved {
                        Ok(())
                    } else {
                        Err(ReqError::Rejected)
                    }
                }
                .boxed()
            })),
            authorize_service: Some(Box::new(move |request: AuthorizeService| {
                async move {
                    if request.service == authorize_uuid {
                        Ok(())
                    } else {
                        Err(ReqError::Rejected)
                    }
                }
                .boxed()
            })),
            ..Default::default()
        };
        let agent_handle = session.register_agent(agent).await?;
        let value = std::sync::Arc::new(pairing_uri);
        let read_value = std::sync::Arc::clone(&value);
        let application = Application {
            services: vec![Service {
                uuid: service_uuid,
                primary: true,
                characteristics: vec![Characteristic {
                    uuid: characteristic_uuid,
                    read: Some(CharacteristicRead {
                        read: true,
                        encrypt_authenticated_read: true,
                        secure_read: true,
                        fun: Box::new(move |request| {
                            let value = std::sync::Arc::clone(&read_value);
                            async move {
                                let offset = usize::from(request.offset);
                                Ok(value.get(offset..).unwrap_or_default().to_vec())
                            }
                            .boxed()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let application_handle = adapter.serve_gatt_application(application).await?;
        let advertisement = Advertisement {
            advertisement_type: Type::Peripheral,
            service_uuids: [service_uuid].into_iter().collect(),
            discoverable: Some(true),
            local_name: Some("AgentPulse Pair".to_owned()),
            ..Default::default()
        };
        let advertisement_handle = adapter.advertise(advertisement).await?;
        Ok(BleHandles {
            _session: session,
            _agent: agent_handle,
            _application: application_handle,
            _advertisement: advertisement_handle,
        })
    }

    fn confirm_numeric(device: String, passkey: u32) -> bool {
        print!(
            "Bluetooth device {device} shows {:06}. Confirm the same number? [y/N] ",
            passkey
        );
        read_confirmation()
    }

    fn confirm_authorization(device: String) -> bool {
        print!("Authorize Bluetooth pairing from {device}? [y/N] ");
        read_confirmation()
    }

    fn read_confirmation() -> bool {
        if std::io::stdout().flush().is_err() {
            return false;
        }
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .is_ok_and(|_| matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }

    struct BleHandles {
        _session: bluer::Session,
        _agent: bluer::agent::AgentHandle,
        _application: bluer::gatt::local::ApplicationHandle,
        _advertisement: bluer::adv::AdvertisementHandle,
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    /// Placeholder on platforms where QR is the complete pairing path.
    pub struct BlePairingAdvertiser;

    impl BlePairingAdvertiser {
        /// Reports that BLE peripheral mode is Linux-only in this release.
        pub fn start(_pairing_uri: &str) -> Result<Self, String> {
            Err("BLE pairing is currently supported on Linux Hosts only".to_owned())
        }
    }
}

pub use platform::BlePairingAdvertiser;
