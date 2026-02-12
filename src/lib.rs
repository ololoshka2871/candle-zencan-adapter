#![allow(missing_docs)]
#![allow(unsafe_op_in_unsafe_fn)]
#![warn(missing_copy_implementations)]

mod candle_sys;

use std::fmt;
use std::ptr::NonNull;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use zencan_common::{
    messages::{CanError, CanId, CanMessage},
    traits::{AsyncCanReceiver, AsyncCanSender, CanSendError},
};

use crate::candle_sys::{
    candle_channel_count, candle_channel_set_bitrate, candle_channel_start, candle_channel_stop,
    candle_dev_close, candle_dev_free, candle_dev_get, candle_dev_get_path, candle_dev_last_error,
    candle_dev_open, candle_frame_read, candle_frame_send, candle_list_free, candle_list_length,
    candle_list_scan, CandleErr, CandleFrame,
};

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
    unsafe {
        let mut list = std::ptr::null_mut();
        if !candle_list_scan(&mut list) {
            return Err(CandleError::NativeError {
                code: CandleErr::Unknown,
                context: "list_scan",
            });
        }

        let mut length: u8 = 0;
        if !candle_list_length(list, &mut length) {
            let _ = candle_list_free(list);
            return Err(CandleError::NativeError {
                code: CandleErr::Unknown,
                context: "list_length",
            });
        }

        let mut devices = Vec::with_capacity(length as usize);
        for index in 0..length {
            let mut handle = std::ptr::null_mut();
            if !candle_dev_get(list, index, &mut handle) {
                let _ = candle_list_free(list);
                return Err(CandleError::NativeError {
                    code: CandleErr::Unknown,
                    context: "dev_get",
                });
            }

            let path = device_path(handle).unwrap_or_default();
            let channel_count = device_channel_count(handle).unwrap_or(0);
            devices.push(CandleDeviceInfo {
                index,
                path,
                channel_count,
            });

            let _ = candle_dev_free(handle);
        }

        let _ = candle_list_free(list);
        Ok(devices)
    }
}

pub fn open_candle(config: CandleConfig) -> Result<(CandleSender, CandleReceiver), CandleError> {
    let device = open_device(config.device_index)?;

    let channel_count = device_channel_count(device.as_ptr()).unwrap_or(0);
    if config.channel >= channel_count {
        unsafe { candle_dev_free(device.as_ptr()) };
        return Err(CandleError::InvalidChannel(config.channel));
    }

    unsafe {
        if !candle_channel_set_bitrate(device.as_ptr(), config.channel, config.bitrate) {
            let code = CandleErr::from_raw(candle_dev_last_error(device.as_ptr()) as i32);
            let _ = candle_dev_free(device.as_ptr());
            return Err(CandleError::NativeError {
                code,
                context: "channel_set_bitrate",
            });
        }

        if !candle_channel_start(device.as_ptr(), config.channel, 0) {
            let code = CandleErr::from_raw(candle_dev_last_error(device.as_ptr()) as i32);
            let _ = candle_dev_free(device.as_ptr());
            return Err(CandleError::NativeError {
                code,
                context: "channel_start",
            });
        }
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

fn open_device(index: u8) -> Result<NonNull<std::ffi::c_void>, CandleError> {
    unsafe {
        let mut list = std::ptr::null_mut();
        if !candle_list_scan(&mut list) {
            return Err(CandleError::NativeError {
                code: CandleErr::Unknown,
                context: "list_scan",
            });
        }

        let mut length: u8 = 0;
        if !candle_list_length(list, &mut length) {
            let _ = candle_list_free(list);
            return Err(CandleError::NativeError {
                code: CandleErr::Unknown,
                context: "list_length",
            });
        }

        if index >= length {
            let _ = candle_list_free(list);
            return Err(CandleError::InvalidDeviceIndex(index));
        }

        let mut handle = std::ptr::null_mut();
        if !candle_dev_get(list, index, &mut handle) {
            let _ = candle_list_free(list);
            return Err(CandleError::NativeError {
                code: CandleErr::Unknown,
                context: "dev_get",
            });
        }
        let _ = candle_list_free(list);

        if !candle_dev_open(handle) {
            let code = CandleErr::from_raw(candle_dev_last_error(handle) as i32);
            let _ = candle_dev_free(handle);
            return Err(CandleError::NativeError {
                code,
                context: "dev_open",
            });
        }

        Ok(NonNull::new(handle).ok_or(CandleError::ChannelClosed)?)
    }
}

fn device_channel_count(handle: *mut std::ffi::c_void) -> Option<u8> {
    unsafe {
        let mut count: u8 = 0;
        if candle_channel_count(handle, &mut count) {
            Some(count)
        } else {
            None
        }
    }
}

fn device_path(handle: *mut std::ffi::c_void) -> Option<String> {
    let mut buf = [0u16; 255];
    unsafe {
        if candle_dev_get_path(handle, buf.as_mut_ptr()) {
            let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..len]))
        } else {
            None
        }
    }
}

fn worker_loop(
    device: NonNull<std::ffi::c_void>,
    channel: u8,
    cmd_rx: mpsc::Receiver<Command>,
    rx_tx: tokio_mpsc::Sender<ReceiverEvent>,
) {
    let mut cmd_rx = cmd_rx;
    let mut rx_tx = rx_tx;
    let timeout_ms = 10u32;

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Send { frame, reply } => {
                    let result = unsafe { send_frame(device.as_ptr(), channel, frame) };
                    let _ = reply.send(result);
                }
                Command::Shutdown => {
                    unsafe {
                        let _ = candle_channel_stop(device.as_ptr(), channel);
                        let _ = candle_dev_close(device.as_ptr());
                        let _ = candle_dev_free(device.as_ptr());
                    }
                    return;
                }
            }
        }

        match unsafe { read_frame(device.as_ptr(), timeout_ms) } {
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

unsafe fn send_frame(
    device: *mut std::ffi::c_void,
    channel: u8,
    msg: CanMessage,
) -> Result<(), SendError> {
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

    if candle_frame_send(device, channel, &mut frame) {
        Ok(())
    } else {
        Err(SendError {
            message: msg,
            details: format!("send failed: {:?}", CandleErr::from_raw(candle_dev_last_error(device) as i32)),
        })
    }
}

unsafe fn read_frame(
    device: *mut std::ffi::c_void,
    timeout_ms: u32,
) -> Result<Option<CanMessage>, ReceiveError> {
    let mut frame = CandleFrame::default();
    if !candle_frame_read(device, &mut frame, timeout_ms) {
        let code = CandleErr::from_raw(candle_dev_last_error(device) as i32);
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
