// this file is based on candle library  https://github.com/elliotwoods/Candle.NET.git/Candle/candle.c

use std::mem::{size_of, zeroed};
use std::ptr::{self};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
};
use windows::Win32::Devices::Usb::{
    WinUsb_ControlTransfer, WinUsb_Free, WinUsb_GetOverlappedResult, WinUsb_Initialize,
    WinUsb_QueryInterfaceSettings, WinUsb_QueryPipe, WinUsb_ReadPipe, WinUsb_SetPipePolicy,
    WinUsb_WritePipe, UsbdPipeTypeBulk, WINUSB_INTERFACE_HANDLE, WINUSB_PIPE_INFORMATION,
    WINUSB_PIPE_POLICY, WINUSB_SETUP_PACKET,
};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_NO_MORE_ITEMS,
    HANDLE, WIN32_ERROR, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::OVERLAPPED;
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

const CANDLE_MAX_DEVICES: usize = 32;
const CANDLE_URB_COUNT: usize = 30;
const CANDLE_MODE_HW_TIMESTAMP: u32 = 0x10;

const USB_DIR_OUT: u8 = 0x00;
const USB_DIR_IN: u8 = 0x80;
const USB_TYPE_VENDOR: u8 = 0x40;
const USB_RECIP_INTERFACE: u8 = 0x01;

const CANDLE_BREQ_HOST_FORMAT: u8 = 0;
const CANDLE_BREQ_BITTIMING: u8 = 1;
const CANDLE_BREQ_MODE: u8 = 2;
const CANDLE_BREQ_BERR: u8 = 3;
const CANDLE_BREQ_BT_CONST: u8 = 4;
const CANDLE_BREQ_DEVICE_CONFIG: u8 = 5;
const CANDLE_TIMESTAMP_GET: u8 = 6;

const CANDLE_DEVMODE_RESET: u32 = 0;
const CANDLE_DEVMODE_START: u32 = 1;

const CANDLE_GUID: GUID = GUID::from_u128(0xc15b4308_04d3_11e6_b3ea_6057189e6443);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum CandleErr {
    Ok = 0,
    CreateFile = 1,
    WinUsbInitialize = 2,
    QueryInterface = 3,
    QueryPipe = 4,
    ParseIfDescr = 5,
    SetHostFormat = 6,
    GetDeviceInfo = 7,
    GetBittimingConst = 8,
    PrepareRead = 9,
    SetDeviceMode = 10,
    SetBittiming = 11,
    BitrateFclk = 12,
    BitrateUnsupported = 13,
    SendFrame = 14,
    ReadTimeout = 15,
    ReadWait = 16,
    ReadResult = 17,
    ReadSize = 18,
    SetupDiIfDetails = 19,
    SetupDiIfDetails2 = 20,
    Malloc = 21,
    PathLen = 22,
    Clsid = 23,
    GetDevices = 24,
    SetupDiIfEnum = 25,
    SetTimestampMode = 26,
    DevOutOfRange = 27,
    GetTimestamp = 28,
    SetPipeRawIo = 29,
    Unknown = 9999,
}

impl CandleErr {
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            0 => CandleErr::Ok,
            1 => CandleErr::CreateFile,
            2 => CandleErr::WinUsbInitialize,
            3 => CandleErr::QueryInterface,
            4 => CandleErr::QueryPipe,
            5 => CandleErr::ParseIfDescr,
            6 => CandleErr::SetHostFormat,
            7 => CandleErr::GetDeviceInfo,
            8 => CandleErr::GetBittimingConst,
            9 => CandleErr::PrepareRead,
            10 => CandleErr::SetDeviceMode,
            11 => CandleErr::SetBittiming,
            12 => CandleErr::BitrateFclk,
            13 => CandleErr::BitrateUnsupported,
            14 => CandleErr::SendFrame,
            15 => CandleErr::ReadTimeout,
            16 => CandleErr::ReadWait,
            17 => CandleErr::ReadResult,
            18 => CandleErr::ReadSize,
            19 => CandleErr::SetupDiIfDetails,
            20 => CandleErr::SetupDiIfDetails2,
            21 => CandleErr::Malloc,
            22 => CandleErr::PathLen,
            23 => CandleErr::Clsid,
            24 => CandleErr::GetDevices,
            25 => CandleErr::SetupDiIfEnum,
            26 => CandleErr::SetTimestampMode,
            27 => CandleErr::DevOutOfRange,
            28 => CandleErr::GetTimestamp,
            29 => CandleErr::SetPipeRawIo,
            _ => CandleErr::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct CandleHostConfig {
    byte_order: u32,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed)]
struct CandleDeviceConfig {
    reserved1: u8,
    reserved2: u8,
    reserved3: u8,
    icount: u8,
    sw_version: u32,
    hw_version: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
struct CandleDeviceMode {
    mode: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed)]
struct CandleBitTiming {
    prop_seg: u32,
    phase_seg1: u32,
    phase_seg2: u32,
    sjw: u32,
    brp: u32,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed)]
pub struct CandleCapability {
    feature: u32,
    fclk_can: u32,
    tseg1_min: u32,
    tseg1_max: u32,
    tseg2_min: u32,
    tseg2_max: u32,
    sjw_max: u32,
    brp_min: u32,
    brp_max: u32,
    brp_inc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum CandleDevState {
    Avail = 0,
    InUse = 1,
}

#[derive(Clone, Copy)]
struct CandleRxUrb {
    ovl: OVERLAPPED,
    buf: [u8; 128],
}

impl Default for CandleRxUrb {
    fn default() -> Self {
        Self {
            ovl: unsafe { zeroed() },
            buf: [0; 128],
        }
    }
}

#[derive(Clone, Copy)]
struct CandleDevice {
    path: [u16; 256],
    state: CandleDevState,
    last_error: CandleErr,
    device_handle: HANDLE,
    winusb_handle: WINUSB_INTERFACE_HANDLE,
    interface_number: u8,
    bulk_in_pipe: u8,
    bulk_out_pipe: u8,
    dconf: CandleDeviceConfig,
    bt_const: CandleCapability,
    rxurbs: [CandleRxUrb; CANDLE_URB_COUNT],
    rxevents: [HANDLE; CANDLE_URB_COUNT],
}

impl Default for CandleDevice {
    fn default() -> Self {
        Self {
            path: [0; 256],
            state: CandleDevState::Avail,
            last_error: CandleErr::Ok,
            device_handle: HANDLE(0),
            winusb_handle: WINUSB_INTERFACE_HANDLE::default(),
            interface_number: 0,
            bulk_in_pipe: 0,
            bulk_out_pipe: 0,
            dconf: CandleDeviceConfig::default(),
            bt_const: CandleCapability::default(),
            rxurbs: [CandleRxUrb::default(); CANDLE_URB_COUNT],
            rxevents: [HANDLE(0); CANDLE_URB_COUNT],
        }
    }
}

struct CandleList {
    num_devices: u8,
    last_error: CandleErr,
    devices: [CandleDevice; CANDLE_MAX_DEVICES],
}

impl Default for CandleList {
    fn default() -> Self {
        Self {
            num_devices: 0,
            last_error: CandleErr::Ok,
            devices: [CandleDevice::default(); CANDLE_MAX_DEVICES],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CandleFrame {
    pub echo_id: u32,
    pub can_id: u32,
    pub can_dlc: u8,
    pub channel: u8,
    pub flags: u8,
    pub reserved: u8,
    pub data: [u8; 8],
    pub timestamp_us: u32,
}

impl Default for CandleFrame {
    fn default() -> Self {
        Self {
            echo_id: 0,
            can_id: 0,
            can_dlc: 0,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0; 8],
            timestamp_us: 0,
        }
    }
}

pub unsafe fn candle_list_scan(list: *mut *mut std::ffi::c_void) -> bool {
    if list.is_null() {
        return false;
    }

    let mut l = Box::new(CandleList::default());

    let hdi = match SetupDiGetClassDevsW(
        Some(&CANDLE_GUID),
        None,
        None,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
    ) {
        Ok(handle) => handle,
        Err(_) => {
            l.last_error = CandleErr::GetDevices;
            *list = Box::into_raw(l) as *mut std::ffi::c_void;
            return false;
        }
    };

    let mut ok = false;
    for i in 0..CANDLE_MAX_DEVICES {
        let mut interface_data = SP_DEVICE_INTERFACE_DATA::default();
        interface_data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

        if SetupDiEnumDeviceInterfaces(hdi, None, &CANDLE_GUID, i as u32, &mut interface_data).is_ok() {
            if !candle_read_di(hdi, &interface_data, &mut l.devices[i]) {
                l.last_error = l.devices[i].last_error;
                ok = false;
                break;
            }
        } else {
            if Some(ERROR_NO_MORE_ITEMS) == get_last_err_code() {
                l.num_devices = i as u8;
                l.last_error = CandleErr::Ok;
                ok = true;
            } else {
                l.last_error = CandleErr::SetupDiIfEnum;
                ok = false;
            }
            break;
        }
    }

    SetupDiDestroyDeviceInfoList(hdi);
    *list = Box::into_raw(l) as *mut std::ffi::c_void;
    ok
}

pub unsafe fn candle_list_free(list: *mut std::ffi::c_void) -> bool {
    if list.is_null() {
        return false;
    }
    drop(Box::from_raw(list as *mut CandleList));
    true
}

pub unsafe fn candle_list_length(list: *mut std::ffi::c_void, length: *mut u8) -> bool {
    if list.is_null() || length.is_null() {
        return false;
    }
    let l = &*(list as *mut CandleList);
    *length = l.num_devices;
    true
}

pub unsafe fn candle_dev_get(
    list: *mut std::ffi::c_void,
    dev_num: u8,
    device: *mut *mut std::ffi::c_void,
) -> bool {
    if list.is_null() || device.is_null() {
        return false;
    }

    let l = &mut *(list as *mut CandleList);
    if dev_num as usize >= CANDLE_MAX_DEVICES {
        l.last_error = CandleErr::DevOutOfRange;
        return false;
    }

    let mut dev = CandleDevice::default();
    dev.path = l.devices[dev_num as usize].path;
    dev.state = l.devices[dev_num as usize].state;
    dev.last_error = CandleErr::Ok;

    let boxed = Box::new(dev);
    *device = Box::into_raw(boxed) as *mut std::ffi::c_void;
    l.last_error = CandleErr::Ok;
    true
}

pub unsafe fn candle_dev_get_state(
    device: *mut std::ffi::c_void,
    state: *mut u32,
) -> bool {
    if device.is_null() || state.is_null() {
        return false;
    }
    let dev = &*(device as *mut CandleDevice);
    *state = dev.state as u32;
    true
}

pub unsafe fn candle_dev_get_path(device: *mut std::ffi::c_void, path: *mut u16) -> bool {
    if device.is_null() || path.is_null() {
        return false;
    }
    let dev = &*(device as *mut CandleDevice);
    ptr::copy_nonoverlapping(dev.path.as_ptr(), path, dev.path.len());
    true
}

pub unsafe fn candle_dev_open(device: *mut std::ffi::c_void) -> bool {
    if device.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    if candle_dev_internal_open(dev) {
        for i in 0..CANDLE_URB_COUNT {
            if let Ok(ev) = CreateEventW(None, true, false, None) {
                dev.rxevents[i] = ev;
                dev.rxurbs[i].ovl.hEvent = ev;
                if !candle_prepare_read(dev, i) {
                    candle_close_rxurbs(dev);
                    return false;
                }
            } else {
                candle_close_rxurbs(dev);
                return false;
            }
        }
        dev.last_error = CandleErr::Ok;
        true
    } else {
        false
    }
}

pub unsafe fn candle_dev_get_timestamp_us(
    device: *mut std::ffi::c_void,
    timestamp_us: *mut u32,
) -> bool {
    if device.is_null() || timestamp_us.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    candle_ctrl_get_timestamp(dev, timestamp_us)
}

pub unsafe fn candle_dev_close(device: *mut std::ffi::c_void) -> bool {
    if device.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    candle_close_rxurbs(dev);
    WinUsb_Free(dev.winusb_handle);
    dev.winusb_handle = WINUSB_INTERFACE_HANDLE::default();
    CloseHandle(dev.device_handle);
    dev.device_handle = HANDLE(0);
    dev.last_error = CandleErr::Ok;
    true
}

pub unsafe fn candle_dev_free(device: *mut std::ffi::c_void) -> bool {
    if device.is_null() {
        return false;
    }
    drop(Box::from_raw(device as *mut CandleDevice));
    true
}

pub unsafe fn candle_dev_last_error(device: *mut std::ffi::c_void) -> i32 {
    if device.is_null() {
        return CandleErr::Unknown as i32;
    }
    let dev = &*(device as *mut CandleDevice);
    dev.last_error as i32
}

pub unsafe fn candle_channel_count(device: *mut std::ffi::c_void, num_channels: *mut u8) -> bool {
    if device.is_null() || num_channels.is_null() {
        return false;
    }
    let dev = &*(device as *mut CandleDevice);
    *num_channels = dev.dconf.icount.saturating_add(1);
    true
}

pub unsafe fn candle_channel_get_capabilities(
    device: *mut std::ffi::c_void,
    channel: u8,
    cap: *mut CandleCapability,
) -> bool {
    if device.is_null() || cap.is_null() {
        return false;
    }
    let dev = &*(device as *mut CandleDevice);
    if channel > dev.dconf.icount {
        return false;
    }
    ptr::copy_nonoverlapping(&dev.bt_const as *const CandleCapability, cap, 1);
    true
}

pub unsafe fn candle_channel_set_timing(
    device: *mut std::ffi::c_void,
    channel: u8,
    data: *const CandleBitTiming,
) -> bool {
    if device.is_null() || data.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    candle_ctrl_set_bittiming(dev, channel, &*data)
}

pub unsafe fn candle_channel_set_bitrate(
    device: *mut std::ffi::c_void,
    channel: u8,
    bitrate: u32,
) -> bool {
    if device.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);

    if dev.bt_const.fclk_can != 48_000_000 {
        dev.last_error = CandleErr::BitrateFclk;
        return false;
    }

    let mut t = CandleBitTiming {
        prop_seg: 1,
        sjw: 1,
        phase_seg1: 13 - 1,
        phase_seg2: 2,
        brp: 0,
    };

    match bitrate {
        10_000 => t.brp = 300,
        20_000 => t.brp = 150,
        50_000 => t.brp = 60,
        83_333 => t.brp = 36,
        100_000 => t.brp = 30,
        125_000 => t.brp = 24,
        250_000 => t.brp = 12,
        500_000 => t.brp = 6,
        800_000 => {
            t.brp = 4;
            t.phase_seg1 = 12 - 1;
            t.phase_seg2 = 2;
        }
        1_000_000 => t.brp = 3,
        _ => {
            dev.last_error = CandleErr::BitrateUnsupported;
            return false;
        }
    }

    candle_ctrl_set_bittiming(dev, channel, &t)
}

pub unsafe fn candle_channel_start(device: *mut std::ffi::c_void, channel: u8, flags: u32) -> bool {
    if device.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    let flags = flags | CANDLE_MODE_HW_TIMESTAMP;
    candle_ctrl_set_device_mode(dev, channel, CANDLE_DEVMODE_START, flags)
}

pub unsafe fn candle_channel_stop(device: *mut std::ffi::c_void, channel: u8) -> bool {
    if device.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    candle_ctrl_set_device_mode(dev, channel, CANDLE_DEVMODE_RESET, 0)
}

pub unsafe fn candle_frame_send(
    device: *mut std::ffi::c_void,
    channel: u8,
    frame: *mut CandleFrame,
) -> bool {
    if device.is_null() || frame.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);
    (*frame).echo_id = 0;
    (*frame).channel = channel;

    let mut bytes_sent = 0u32;
    let frame_slice = std::slice::from_raw_parts(frame as *const u8, size_of::<CandleFrame>());
    let rc = WinUsb_WritePipe(
        dev.winusb_handle,
        dev.bulk_out_pipe,
        frame_slice,
        Some(&mut bytes_sent),
        None,
    );

    dev.last_error = if rc.is_ok() {
        CandleErr::Ok
    } else {
        CandleErr::SendFrame
    };
    rc.is_ok()
}

pub unsafe fn candle_frame_read(
    device: *mut std::ffi::c_void,
    frame: *mut CandleFrame,
    timeout_ms: u32,
) -> bool {
    if device.is_null() || frame.is_null() {
        return false;
    }
    let dev = &mut *(device as *mut CandleDevice);

    let wait_result = WaitForMultipleObjects(
        &dev.rxevents,
        false,
        timeout_ms,
    );

    if wait_result == WAIT_TIMEOUT {
        dev.last_error = CandleErr::ReadTimeout;
        return false;
    }

    if wait_result.0 < WAIT_OBJECT_0.0 || wait_result.0 >= (WAIT_OBJECT_0.0 + CANDLE_URB_COUNT as u32) {
        dev.last_error = CandleErr::ReadWait;
        return false;
    }

    let urb_num = (wait_result.0 - WAIT_OBJECT_0.0) as usize;
    let mut bytes_transferred = 0u32;
    let rc = WinUsb_GetOverlappedResult(
        dev.winusb_handle,
        &mut dev.rxurbs[urb_num].ovl,
        &mut bytes_transferred,
        false,
    );

    if rc.is_err() {
        let _ = candle_prepare_read(dev, urb_num);
        dev.last_error = CandleErr::ReadResult;
        return false;
    }

    if bytes_transferred < (size_of::<CandleFrame>() as u32).saturating_sub(4) {
        let _ = candle_prepare_read(dev, urb_num);
        dev.last_error = CandleErr::ReadSize;
        return false;
    }

    if bytes_transferred < size_of::<CandleFrame>() as u32 {
        (*frame).timestamp_us = 0;
    }

    ptr::copy_nonoverlapping(
        dev.rxurbs[urb_num].buf.as_ptr(),
        frame as *mut u8,
        size_of::<CandleFrame>(),
    );

    candle_prepare_read(dev, urb_num)
}

fn candle_read_di(
    hdi: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    interface_data: &SP_DEVICE_INTERFACE_DATA,
    dev: &mut CandleDevice,
) -> bool {
    let mut required_length = 0u32;
    let _ = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            hdi,
            interface_data,
            None,
            0,
            Some(&mut required_length),
            None,
        )
    };

    if get_last_err_code() == Some(ERROR_INSUFFICIENT_BUFFER) {
        dev.last_error = CandleErr::SetupDiIfDetails;
        return false;
    }

    let mut buffer = vec![0u8; required_length as usize];
    let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    }

    let mut length = required_length;
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            hdi,
            interface_data,
            Some(detail),
            length,
            Some(&mut length),
            None,
        )
    }
    .is_ok();

    if !ok {
        dev.last_error = CandleErr::SetupDiIfDetails2;
        return false;
    }

    unsafe {
        let path_ptr = (*detail).DevicePath.as_ptr();
        let mut i = 0usize;
        while i < dev.path.len() {
            let val = *path_ptr.add(i);
            dev.path[i] = val;
            if val == 0 {
                break;
            }
            i += 1;
        }
    }

    if candle_dev_internal_open(dev) {
        dev.state = CandleDevState::Avail;
        let _ = unsafe { candle_dev_close(dev as *mut CandleDevice as *mut std::ffi::c_void) };
    } else {
        dev.state = CandleDevState::InUse;
    }

    dev.last_error = CandleErr::Ok;
    true
}

fn candle_dev_internal_open(dev: &mut CandleDevice) -> bool {
    let path_ptr = PCWSTR(dev.path.as_ptr());
    let handle = unsafe {
        CreateFileW(
            path_ptr,
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            None,
        )
    };

    let handle = if let Ok(h) = handle {
        dev.device_handle = h;
        h
    } else {
        dev.last_error = CandleErr::CreateFile;
        return false;
    };

    let mut winusb_handle = WINUSB_INTERFACE_HANDLE::default();
    let rc = unsafe { WinUsb_Initialize(handle, &mut winusb_handle) };
    if rc.is_err() {
        dev.last_error = CandleErr::WinUsbInitialize;
        unsafe { CloseHandle(handle) };
        return false;
    }
    dev.winusb_handle = winusb_handle;

    let mut iface_descriptor = unsafe { zeroed() };
    let rc = unsafe { WinUsb_QueryInterfaceSettings(winusb_handle, 0, &mut iface_descriptor) };
    if rc.is_err() {
        dev.last_error = CandleErr::QueryInterface;
        unsafe { WinUsb_Free(winusb_handle) };
        unsafe { CloseHandle(handle) };
        return false;
    }
    dev.interface_number = iface_descriptor.bInterfaceNumber;

    let mut pipes_found = 0;
    for i in 0..iface_descriptor.bNumEndpoints {
        let mut pipe_info = WINUSB_PIPE_INFORMATION::default();
        let rc = unsafe { WinUsb_QueryPipe(winusb_handle, 0, i, &mut pipe_info) };
        if rc.is_err() {
            dev.last_error = CandleErr::QueryPipe;
            unsafe { WinUsb_Free(winusb_handle) };
            unsafe { CloseHandle(handle) };
            return false;
        }

        let is_bulk = pipe_info.PipeType == UsbdPipeTypeBulk;
        let is_in = (pipe_info.PipeId & 0x80) != 0;

        if is_bulk && is_in {
            dev.bulk_in_pipe = pipe_info.PipeId;
            pipes_found += 1;
        } else if is_bulk {
            dev.bulk_out_pipe = pipe_info.PipeId;
            pipes_found += 1;
        } else {
            dev.last_error = CandleErr::ParseIfDescr;
            unsafe { WinUsb_Free(winusb_handle) };
            unsafe { CloseHandle(handle) };
            return false;
        }
    }

    if pipes_found != 2 {
        dev.last_error = CandleErr::ParseIfDescr;
        unsafe { WinUsb_Free(winusb_handle) };
        unsafe { CloseHandle(handle) };
        return false;
    }

    let use_raw_io: u8 = 1;
    let rc = unsafe {
        WinUsb_SetPipePolicy(
            winusb_handle,
            dev.bulk_in_pipe,
            WINUSB_PIPE_POLICY(7u32), //WINUSB_PIPE_POLICY::RAW_IO
            size_of::<u8>() as u32,
            &use_raw_io as *const u8 as *const _,
        )
    };

    if rc.is_err() {
        dev.last_error = CandleErr::SetPipeRawIo;
        unsafe { WinUsb_Free(winusb_handle) };
        unsafe { CloseHandle(handle) };
        return false;
    }

    if !candle_ctrl_set_host_format(dev) {
        unsafe { WinUsb_Free(winusb_handle) };
        unsafe { CloseHandle(handle) };
        return false;
    }

    if !candle_ctrl_get_config(dev, &mut dev.dconf) {
        unsafe { WinUsb_Free(winusb_handle) };
        unsafe { CloseHandle(handle) };
        return false;
    }

    if !candle_ctrl_get_capability(dev, 0, &mut dev.bt_const) {
        dev.last_error = CandleErr::GetBittimingConst;
        unsafe { WinUsb_Free(winusb_handle) };
        unsafe { CloseHandle(handle) };
        return false;
    }

    dev.last_error = CandleErr::Ok;
    true
}

fn candle_prepare_read(dev: &mut CandleDevice, urb_num: usize) -> bool {
    let mut bytes_read = 0u32;
    let rc = unsafe {
        WinUsb_ReadPipe(
            dev.winusb_handle,
            dev.bulk_in_pipe,
            Some(&mut dev.rxurbs[urb_num].buf),
            Some(&mut bytes_read),
            Some(&dev.rxurbs[urb_num].ovl as *const OVERLAPPED),
        )
    };

    if rc.is_ok() || get_last_err_code() == Some(ERROR_IO_PENDING) {
        dev.last_error = CandleErr::PrepareRead;
        false
    } else {
        dev.last_error = CandleErr::Ok;
        true
    }
}

fn candle_close_rxurbs(dev: &mut CandleDevice) -> bool {
    for ev in dev.rxevents.iter() {
        if !ev.is_invalid() {
            unsafe {
                let _ = CloseHandle(*ev);
            }
        }
    }
    true
}

fn usb_control_msg(
    hnd: WINUSB_INTERFACE_HANDLE,
    request: u8,
    request_type: u8,
    value: u16,
    index: u16,
    data: *mut u8,
    size: u16,
) -> bool {
    let packet = WINUSB_SETUP_PACKET {
        RequestType: request_type,
        Request: request,
        Value: value,
        Index: index,
        Length: size,
    };

    let mut bytes_sent = 0u32;
    let buffer = unsafe { std::slice::from_raw_parts_mut(data, size as usize) };
    unsafe {
        WinUsb_ControlTransfer(
            hnd,
            packet,
            Some(buffer),
            Some(&mut bytes_sent),
            None,
        )
    }
    .is_ok()
}

fn candle_ctrl_set_host_format(dev: &mut CandleDevice) -> bool {
    let mut conf = CandleHostConfig { byte_order: 0x0000_beef };
    let rc = usb_control_msg(
        dev.winusb_handle,
        CANDLE_BREQ_HOST_FORMAT,
        USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
        1,
        dev.interface_number as u16,
        &mut conf as *mut CandleHostConfig as *mut u8,
        size_of::<CandleHostConfig>() as u16,
    );

    dev.last_error = if rc { CandleErr::Ok } else { CandleErr::SetHostFormat };
    rc
}

fn candle_ctrl_set_device_mode(
    dev: &mut CandleDevice,
    channel: u8,
    mode: u32,
    flags: u32,
) -> bool {
    let mut dm = CandleDeviceMode { mode, flags };
    let rc = usb_control_msg(
        dev.winusb_handle,
        CANDLE_BREQ_MODE,
        USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
        channel as u16,
        dev.interface_number as u16,
        &mut dm as *mut CandleDeviceMode as *mut u8,
        size_of::<CandleDeviceMode>() as u16,
    );

    dev.last_error = if rc { CandleErr::Ok } else { CandleErr::SetDeviceMode };
    rc
}

fn candle_ctrl_get_config(dev: &mut CandleDevice, dconf: &mut CandleDeviceConfig) -> bool {
    let rc = usb_control_msg(
        dev.winusb_handle,
        CANDLE_BREQ_DEVICE_CONFIG,
        USB_DIR_IN | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
        1,
        dev.interface_number as u16,
        dconf as *mut CandleDeviceConfig as *mut u8,
        size_of::<CandleDeviceConfig>() as u16,
    );

    dev.last_error = if rc { CandleErr::Ok } else { CandleErr::GetDeviceInfo };
    rc
}

fn candle_ctrl_get_timestamp(dev: &mut CandleDevice, current_timestamp: *mut u32) -> bool {
    let rc = usb_control_msg(
        dev.winusb_handle,
        CANDLE_TIMESTAMP_GET,
        USB_DIR_IN | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
        1,
        dev.interface_number as u16,
        current_timestamp as *mut u8,
        size_of::<u32>() as u16,
    );

    dev.last_error = if rc { CandleErr::Ok } else { CandleErr::GetTimestamp };
    rc
}

fn candle_ctrl_get_capability(
    dev: &mut CandleDevice,
    channel: u8,
    data: &mut CandleCapability,
) -> bool {
    let rc = usb_control_msg(
        dev.winusb_handle,
        CANDLE_BREQ_BT_CONST,
        USB_DIR_IN | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
        channel as u16,
        0,
        data as *mut CandleCapability as *mut u8,
        size_of::<CandleCapability>() as u16,
    );

    dev.last_error = if rc { CandleErr::Ok } else { CandleErr::GetBittimingConst };
    rc
}

fn candle_ctrl_set_bittiming(
    dev: &mut CandleDevice,
    channel: u8,
    data: &CandleBitTiming,
) -> bool {
    let rc = usb_control_msg(
        dev.winusb_handle,
        CANDLE_BREQ_BITTIMING,
        USB_DIR_OUT | USB_TYPE_VENDOR | USB_RECIP_INTERFACE,
        channel as u16,
        0,
        data as *const CandleBitTiming as *mut u8,
        size_of::<CandleBitTiming>() as u16,
    );

    dev.last_error = if rc { CandleErr::Ok } else { CandleErr::SetBittiming };
    rc
}

fn get_last_err_code() -> Option<WIN32_ERROR> {
    let err = unsafe { GetLastError() };
    err.err().map(|e| WIN32_ERROR(e.code().0 as u32))
}