#![allow(missing_docs)]

mod candle_sys;

use std::fmt;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use zencan_common::{
    messages::{CanError, CanId, CanMessage},
    traits::{AsyncCanReceiver, AsyncCanSender, CanSendError},
};

pub use crate::candle_sys::{CandleDevice, CandleErr, CandleFrame, CandleList, CANDLE_GUID};

#[derive(Debug, Clone, Copy)]
pub struct CandleConfig {
    pub device_index: u8,
    pub channel: u8,
    pub bitrate: u32,
}

#[derive(Debug, Clone)]
pub struct CandleDeviceInfo {
    pub index: u8,
    pub path: String,
    pub channel_count: u8,
}

#[derive(Debug)]
pub enum CandleError {
    NativeError { code: CandleErr, context: &'static str },
    InvalidDeviceIndex(u8),
    InvalidChannel(u8),
    ChannelClosed,
}

#[derive(Debug)]
pub enum ReceiveError {
    NativeError { code: CandleErr },
    CanError { source: CanError },
    ChannelClosed,
}

#[derive(Debug)]
pub struct SendError {
    message: CanMessage,
    details: String,
}

impl CanSendError for SendError {
    fn into_can_message(self) -> CanMessage {
        self.message
    }

    fn message(&self) -> String {
        self.details.clone()
    }
}

#[derive(Debug)]
pub struct CandleSender {
    inner: Arc<Inner>,
}

#[derive(Debug)]
pub struct CandleReceiver {
    rx: tokio_mpsc::Receiver<ReceiverEvent>,
    pending_error: Option<ReceiveError>,
}

#[derive(Debug)]
struct Inner {
    cmd_tx: mpsc::Sender<Command>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Debug)]
enum Command {
    Send {
        frame: CanMessage,
        reply: oneshot::Sender<Result<(), SendError>>,
    },
    Shutdown,
}

type ReceiverEvent = Result<CanMessage, ReceiveError>;

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
        if let Some(handle) = self.join.lock().ok().and_then(|mut h| h.take()) {
            let _ = handle.join();
        }
    }
}

impl AsyncCanSender for CandleSender {
    type Error = SendError;

    async fn send(&mut self, msg: CanMessage) -> Result<(), Self::Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inner
            .cmd_tx
            .send(Command::Send {
                frame: msg,
                reply: reply_tx,
            })
            .map_err(|_| SendError {
                message: msg,
                details: "worker closed".to_string(),
            })?;

        match reply_rx.await {
            Ok(result) => result,
            Err(_) => Err(SendError {
                message: msg,
                details: "worker closed".to_string(),
            }),
        }
    }
}

impl AsyncCanReceiver for CandleReceiver {
    type Error = ReceiveError;

    fn try_recv(&mut self) -> Option<CanMessage> {
        if self.pending_error.is_some() {
            return None;
        }
        match self.rx.try_recv() {
            Ok(Ok(msg)) => Some(msg),
            Ok(Err(err)) => {
                self.pending_error = Some(err);
                None
            }
            Err(_) => None,
        }
    }

    async fn recv(&mut self) -> Result<CanMessage, Self::Error> {
        if let Some(err) = self.pending_error.take() {
            return Err(err);
        }
        match self.rx.recv().await {
            Some(Ok(msg)) => Ok(msg),
            Some(Err(err)) => Err(err),
            None => Err(ReceiveError::ChannelClosed),
        }
    }
}

pub fn list_devices() -> Result<Vec<CandleDeviceInfo>, CandleError> {
    let list = CandleList::scan().map_err(|code| CandleError::NativeError {
        code,
        context: "list_scan",
    })?;

    let length = list.len();
    let mut devices = Vec::with_capacity(length as usize);
    for index in 0..length {
        let device = list.device(index).map_err(|code| CandleError::NativeError {
            code,
            context: "dev_get",
        })?;

        devices.push(CandleDeviceInfo {
            index,
            path: device.path_string(),
            channel_count: device.channel_count(),
        });
    }

    Ok(devices)
}

pub fn open_candle(config: CandleConfig) -> Result<(CandleSender, CandleReceiver), CandleError> {
    let mut device = open_device(config.device_index)?;

    let channel_count = device.channel_count();
    if config.channel >= channel_count {
        device.close();
        return Err(CandleError::InvalidChannel(config.channel));
    }

    if let Err(code) = device.channel_set_bitrate(config.channel, config.bitrate) {
        device.close();
        return Err(CandleError::NativeError {
            code,
            context: "channel_set_bitrate",
        });
    }

    if let Err(code) = device.channel_start(config.channel, 0) {
        device.close();
        return Err(CandleError::NativeError {
            code,
            context: "channel_start",
        });
    }

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (rx_tx, rx_rx) = tokio_mpsc::channel(1024);

    let channel = config.channel;
    let handle = thread::spawn(move || worker_loop(device, channel, cmd_rx, rx_tx));

    let inner = Arc::new(Inner {
        cmd_tx,
        join: Mutex::new(Some(handle)),
    });

    Ok((
        CandleSender { inner: inner.clone() },
        CandleReceiver {
            rx: rx_rx,
            pending_error: None,
        },
    ))
}

fn open_device(index: u8) -> Result<CandleDevice, CandleError> {
    let list = CandleList::scan().map_err(|code| CandleError::NativeError {
        code,
        context: "list_scan",
    })?;

    let length = list.len();
    if index >= length {
        return Err(CandleError::InvalidDeviceIndex(index));
    }

    let mut device = list.device(index).map_err(|code| CandleError::NativeError {
        code,
        context: "dev_get",
    })?;

    if let Err(code) = device.open() {
        return Err(CandleError::NativeError {
            code,
            context: "dev_open",
        });
    }

    Ok(device)
}

fn worker_loop(
    mut device: CandleDevice,
    channel: u8,
    cmd_rx: mpsc::Receiver<Command>,
    rx_tx: tokio_mpsc::Sender<ReceiverEvent>,
) {
    let timeout_ms = 10u32;

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Send { frame, reply } => {
                    let result = send_frame(&mut device, channel, frame);
                    let _ = reply.send(result);
                }
                Command::Shutdown => {
                    let _ = device.channel_stop(channel);
                    device.close();
                    return;
                }
            }
        }

        match read_frame(&mut device, timeout_ms) {
            Ok(Some(msg)) => {
                let _ = rx_tx.blocking_send(Ok(msg));
            }
            Ok(None) => {}
            Err(err) => {
                let _ = rx_tx.blocking_send(Err(err));
            }
        }
    }
}

fn send_frame(device: &mut CandleDevice, channel: u8, msg: CanMessage) -> Result<(), SendError> {
    let mut frame = CandleFrame::default();
    frame.can_id = msg.id().raw();
    if msg.id().is_extended() {
        frame.can_id |= CANDLE_ID_EXTENDED;
    }
    if msg.is_rtr() {
        frame.can_id |= CANDLE_ID_RTR;
    }
    frame.can_dlc = msg.dlc;
    frame.channel = channel;
    frame.data[..msg.dlc as usize].copy_from_slice(msg.data());

    if device.frame_send(channel, &mut frame).is_ok() {
        Ok(())
    } else {
        Err(SendError {
            message: msg,
            details: format!("send failed: {:?}", device.last_error()),
        })
    }
}

fn read_frame(device: &mut CandleDevice, timeout_ms: u32) -> Result<Option<CanMessage>, ReceiveError> {
    let mut frame = CandleFrame::default();
    if let Err(code) = device.frame_read(&mut frame, timeout_ms) {
        if code == CandleErr::ReadTimeout {
            return Ok(None);
        }
        return Err(ReceiveError::NativeError { code });
    }

    let is_error = (frame.can_id & CANDLE_ID_ERR) != 0;
    if is_error {
        return Err(ReceiveError::CanError {
            source: CanError::Other,
        });
    }

    let id = if (frame.can_id & CANDLE_ID_EXTENDED) != 0 {
        CanId::extended((frame.can_id & 0x1FFF_FFFF) as u32)
    } else {
        CanId::std((frame.can_id & 0x7FF) as u16)
    };

    let msg = if (frame.can_id & CANDLE_ID_RTR) != 0 {
        CanMessage::new_rtr(id)
    } else {
        CanMessage::new(id, &frame.data[..frame.can_dlc as usize])
    };

    Ok(Some(msg))
}

impl fmt::Display for CandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandleError::NativeError { code, context } => {
                write!(f, "native error {code:?} at {context}")
            }
            CandleError::InvalidDeviceIndex(index) => write!(f, "invalid device index {index}"),
            CandleError::InvalidChannel(channel) => write!(f, "invalid channel {channel}"),
            CandleError::ChannelClosed => write!(f, "channel closed"),
        }
    }
}

const CANDLE_ID_EXTENDED: u32 = 0x8000_0000;
const CANDLE_ID_RTR: u32 = 0x4000_0000;
const CANDLE_ID_ERR: u32 = 0x2000_0000;
