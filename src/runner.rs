// Jackson Coxson

use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};

use idevice::{
    core_device_proxy::CoreDeviceProxy,
    lockdownd::LockdowndClient,
    usbmuxd::{Connection, UsbmuxdConnection, UsbmuxdDevice},
    IdeviceError, ReadWrite,
};
use log::{info, warn};
use serde::Serialize;
use serde_json::Map;
use tun_rs::AbstractDevice;

#[derive(Clone)]
pub struct Runner {
    sender: tokio::sync::mpsc::UnboundedSender<RunnerRequest>,
}

enum RunnerRequestType {
    ListTunnels,
    ClearTunnels,
    Cancel(Udid),
    StartTunnel(Udid, IpAddr),
}
enum RunnerResponse {
    ListTunnels(String),
    Ok,
    Err,
}

#[derive(Serialize)]
struct ListTunnelsTunnel {
    #[serde(rename = "tunnel-address")]
    tunnel_address: String,
    #[serde(rename = "tunnel-port")]
    tunnel_port: u16,
    interface: String,
}

type RunnerRequest = (
    RunnerRequestType,
    tokio::sync::oneshot::Sender<RunnerResponse>,
);

type Udid = String;
type DeviceCache = std::collections::HashMap<Udid, CachedDevice>;

struct CachedDevice {
    killer: tokio::sync::oneshot::Sender<()>,
    killed: tokio::sync::oneshot::Receiver<()>,
    server_addr: IpAddr,
    rsd_port: u16,
}

impl Runner {
    pub async fn list_tunnels(&self) -> Option<String> {
        let (sender, recv) = tokio::sync::oneshot::channel();
        if let Err(e) = self.sender.send((RunnerRequestType::ListTunnels, sender)) {
            log::error!("Failed to send request to runner: {e:?}");
            return None;
        }

        match recv.await {
            Ok(RunnerResponse::ListTunnels(s)) => Some(s),
            _ => {
                log::error!("Unexpected runner response!");
                None
            }
        }
    }

    pub async fn clear_tunnels(&self) -> bool {
        let (sender, recv) = tokio::sync::oneshot::channel();
        if let Err(e) = self.sender.send((RunnerRequestType::ClearTunnels, sender)) {
            log::error!("Failed to send request to runner: {e:?}");
            return false;
        }

        match recv.await {
            Ok(RunnerResponse::Ok) => true,
            _ => {
                log::error!("Unexpected runner response!");
                false
            }
        }
    }

    pub async fn cancel_tunnel(&self, udid: String) -> bool {
        let (sender, recv) = tokio::sync::oneshot::channel();
        if let Err(e) = self.sender.send((RunnerRequestType::Cancel(udid), sender)) {
            log::error!("Failed to send request to runner: {e:?}");
            return false;
        }

        match recv.await {
            Ok(RunnerResponse::Ok) => true,
            _ => {
                log::error!("Unexpected runner response!");
                false
            }
        }
    }

    pub async fn start_tunnel(&self, udid: String, ip: IpAddr) -> bool {
        let (sender, recv) = tokio::sync::oneshot::channel();
        if let Err(e) = self
            .sender
            .send((RunnerRequestType::StartTunnel(udid, ip), sender))
        {
            log::error!("Failed to send request to runner: {e:?}");
            return false;
        }

        match recv.await {
            Ok(RunnerResponse::Ok) => true,
            _ => {
                log::error!("Unexpected runner response!");
                false
            }
        }
    }
}

pub async fn start_runner() -> Runner {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<RunnerRequest>();

    tokio::spawn(async move {
        // Check usbmuxd every second for the devices
        // Check them against the list of tunnels created
        // Create a new tunnel for each new device
        // Kill the tunnel of missing devices
        // Read the runner receiver for requests

        let mut cache = DeviceCache::new();

        let mut usbmuxd = loop {
            match idevice::usbmuxd::UsbmuxdConnection::default().await {
                Ok(u) => break u,
                Err(e) => {
                    log::error!("Failed to connect to usbmuxd: {e:?}, trying again...");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        };
        loop {
            let devs = match usbmuxd.get_devices().await {
                Ok(d) => d,
                Err(e) => {
                    log::error!("Unable to get devices from usbmuxd! {e:?}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            for dev in devs.iter() {
                if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(dev.udid.clone())
                {
                    let cached_device = match start_tunnel(dev).await {
                        Ok(c) => c,
                        Err(e) => {
                            log::error!("Failed to create tun for {}: {e:?}", dev.udid);
                            continue;
                        }
                    };
                    e.insert(cached_device);
                }
            }

            let dangling_devices = cache
                .keys()
                .filter(|x| !devs.iter().any(|y| x == &&y.udid))
                .map(|x| x.to_owned())
                .collect::<Vec<String>>();

            for d in dangling_devices {
                if let Some(d) = cache.remove(&d) {
                    d.killer.send(()).ok();
                }
            }

            if let Ok((request, sender)) = receiver.try_recv() {
                match request {
                    RunnerRequestType::ListTunnels => {
                        let mut res = Map::new();
                        for (udid, dev) in &cache {
                            res.insert(
                                udid.to_owned(),
                                serde_json::to_value(vec![ListTunnelsTunnel {
                                    tunnel_address: dev.server_addr.to_string(),
                                    tunnel_port: dev.rsd_port,
                                    interface: "idk".to_string(),
                                }])
                                .unwrap(),
                            );
                        }

                        sender
                            .send(RunnerResponse::ListTunnels(
                                serde_json::to_string(&res).unwrap(),
                            ))
                            .ok();
                    }
                    RunnerRequestType::ClearTunnels => {
                        for dev in cache.keys().map(|x| x.to_owned()).collect::<Vec<String>>() {
                            if let Some(dev) = cache.remove(&dev) {
                                dev.killer.send(()).ok();
                            }
                        }
                    }
                    RunnerRequestType::Cancel(udid) => {
                        if let Some(dev) = cache.remove(&udid) {
                            dev.killer.send(()).ok();
                            sender.send(RunnerResponse::Ok).ok();
                        } else {
                            warn!("Device {udid} was not found to cancel");
                            sender.send(RunnerResponse::Err).ok();
                        }
                    }
                    RunnerRequestType::StartTunnel(udid, ip) => {
                        match start_tunnel(&UsbmuxdDevice {
                            connection_type: Connection::Network(ip),
                            udid: udid.clone(),
                            device_id: 0,
                        })
                        .await
                        {
                            Ok(dev) => {
                                cache.insert(udid, dev);
                                sender.send(RunnerResponse::Ok).ok();
                            }
                            Err(e) => {
                                log::error!("Failed to add device to tunneld: {e:?}");
                                sender.send(RunnerResponse::Err).ok();
                            }
                        }
                    }
                }
            }

            let mut to_remove = Vec::new();

            for (udid, dev) in &mut cache {
                if dev.killed.try_recv().is_ok() {
                    to_remove.push(udid.clone());
                }
            }

            for udid in to_remove {
                cache.remove(&udid);
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });

    Runner { sender }
}

async fn start_tunnel(dev: &UsbmuxdDevice) -> Result<CachedDevice, Box<dyn std::error::Error>> {
    let lockdownd_socket = socket_from_connection_type(
        dev.connection_type.clone(),
        dev.device_id,
        idevice::lockdownd::LOCKDOWND_PORT,
    )
    .await?;

    let mut usbmuxd = UsbmuxdConnection::default().await?;
    let pairing_file = usbmuxd.get_pair_record(dev.udid.as_str()).await?;

    let mut lockdown_client =
        LockdowndClient::new(idevice::Idevice::new(lockdownd_socket, "tunneld"));
    lockdown_client.start_session(&pairing_file).await?;

    let (port, _) = lockdown_client
        .start_service(idevice::core_device_proxy::SERVCE_NAME)
        .await?;

    let proxy_socket =
        socket_from_connection_type(dev.connection_type.clone(), dev.device_id, port).await?;

    let mut idev = idevice::Idevice::new(proxy_socket, "tunneld");
    idev.start_session(&pairing_file).await?;
    let mut tun_proxy = CoreDeviceProxy::new(idev);
    let response = tun_proxy.establish_tunnel().await?;
    let server_address = response.server_address.parse::<IpAddr>()?;
    let udid = dev.udid.clone();

    let dev = tun_rs::create(&tun_rs::Configuration::default())?;
    dev.add_address_v6(response.client_parameters.address.parse()?, 32)?;
    dev.set_mtu(response.client_parameters.mtu)?;
    dev.set_network_address(
        response.client_parameters.address,
        response.client_parameters.netmask.parse()?,
        Some(response.server_address.parse()?),
    )?;

    let async_dev = tun_rs::AsyncDevice::new(dev)?;
    async_dev.enabled(true)?;
    info!(
        "Created tunnel for {udid} - [{:?}] {}:{}",
        async_dev.name(),
        response.server_address,
        response.server_rsd_port
    );

    let (killed_sender, killed_receiver) = tokio::sync::oneshot::channel();
    let (killer_sender, mut killer_receiver) = tokio::sync::oneshot::channel();

    tokio::task::spawn(async move {
        loop {
            let mut buf = vec![0; 1500];
            tokio::select! {
                len = async_dev.recv(&mut buf) => {
                    match len {
                        Ok(len) => {
                            if len == 0 {
                                continue;
                            }
                            if let Err(e) = tun_proxy.send(&buf[..len]).await {
                                warn!("Failed to send packet to device {udid}: {e:?}");
                                killed_sender.send(()).unwrap();
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("tunnel {udid} has stopped: {e:?}");
                            killed_sender.send(()).unwrap();
                            break;
                        }
                    }
                }
                res = tun_proxy.recv() => {
                    match res {
                        Ok(res) => {
                            if res.is_empty() {
                                continue;
                            }
                            if let Err(e) = async_dev.send(&res).await {
                                warn!("Failed to send packet to tun {udid}: {e:?}\n{res:?}");
                            }
                        }
                        Err(e) => {
                            warn!("tunnel {udid} has stopped: {e:?}");
                            killed_sender.send(()).unwrap();
                            break;
                        }
                    }
                }
            }
            if killer_receiver.try_recv().is_ok() {
                break;
            }
        }
        info!("tunnel {udid} stopped");
    });

    Ok(CachedDevice {
        killer: killer_sender,
        killed: killed_receiver,
        server_addr: server_address,
        rsd_port: response.server_rsd_port,
    })
}

async fn socket_from_connection_type(
    con: Connection,
    id: u32,
    port: u16,
) -> Result<Box<dyn ReadWrite>, IdeviceError> {
    Ok(match con {
        idevice::usbmuxd::Connection::Usb => {
            let usbmuxd = UsbmuxdConnection::default().await?;
            usbmuxd.connect_to_device(id, port).await?
        }
        idevice::usbmuxd::Connection::Network(ip) => {
            let socket_addr = match ip {
                std::net::IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, port)),
                std::net::IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port, 0, 0)),
            };
            Box::new(tokio::net::TcpStream::connect(socket_addr).await?)
        }
        idevice::usbmuxd::Connection::Unknown(_) => return Err(IdeviceError::UnexpectedResponse),
    })
}
