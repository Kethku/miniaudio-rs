use crate::lock::{RwLockReadGuard, RwLockWriteGuard, SpinRwLock};
use crate::{Error, Format, FramesMut};
use miniaudio_sys as sys;
use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

#[repr(transparent)]
#[derive(Clone)]
pub struct DecoderConfig(sys::ma_decoder_config);

impl DecoderConfig {
    #[inline]
    pub fn new(format: Format, output_channels: u32, output_sample_rate: u32) -> Self {
        DecoderConfig(unsafe {
            sys::ma_decoder_config_init(format as _, output_channels as _, output_sample_rate as _)
        })
    }
}

#[repr(transparent)]
pub struct RawDecoder {
    inner: sys::ma_decoder,
}

impl RawDecoder {
    #[inline]
    pub fn read_pcm_frames(&mut self, output: &mut FramesMut) -> u64 {
        unsafe {
            sys::ma_decoder_read_pcm_frames(
                &self.inner as *const _ as *mut _,
                output.as_mut_ptr() as *mut _,
                output.frame_count() as u64,
            )
        }
    }

    #[inline]
    pub fn length_in_pcm_frames(&mut self) -> u64 {
        unsafe { sys::ma_decoder_get_length_in_pcm_frames(&self.inner as *const _ as *mut _) }
    }

    #[inline]
    pub fn seek_to_pcm_frame(&mut self, frame_index: u64) -> Result<(), Error> {
        Error::from_c_result(unsafe {
            sys::ma_decoder_seek_to_pcm_frame(&self.inner as *const _ as *mut _, frame_index)
        })
    }

    pub fn output_format(&self) -> Format {
        Format::from_c(self.inner.outputFormat)
    }

    pub fn output_channels(&self) -> u32 {
        self.inner.outputChannels as _
    }

    pub fn output_sample_rate(&self) -> u32 {
        self.inner.outputSampleRate as _
    }
}

impl Drop for RawDecoder {
    fn drop(&mut self) {
        Error::from_c_result(unsafe { sys::ma_decoder_uninit(&mut self.inner) })
            .expect("failed to uninit decoder");
    }
}

/// A decoder with synchronization. This will use a spinlock to synchronize access to the decoder
/// on each function call. The decoder may have multiple readers or one writer. Cloning this
/// decoder will simply return another reference to the same decoder.
pub struct SyncDecoder {
    inner: Arc<SpinRwLock<RawDecoder>>,
}

impl SyncDecoder {
    pub fn from_file(file: &str, config: Option<&DecoderConfig>) -> Result<Self, Error> {
        let decoder = Arc::new(SpinRwLock::new(MaybeUninit::<RawDecoder>::uninit()));

        let filename = CString::new(file.to_string()).map_err(|_err| Error::InvalidFile)?;

        let result = unsafe {
            sys::ma_decoder_init_file(
                filename.as_ptr() as *const _,
                config.map(|c| &c.0 as *const _).unwrap_or(std::ptr::null()),
                Arc::deref(&decoder).as_ptr() as *const _ as *mut _,
            )
        };

        map_result!(
            result,
            SyncDecoder {
                inner: unsafe { std::mem::transmute(decoder) }
            }
        )
    }

    pub fn from_memory(data: &[u8], config: Option<&DecoderConfig>) -> Result<Self, Error> {
        let decoder = Arc::new(SpinRwLock::new(MaybeUninit::<RawDecoder>::uninit()));

        let result = unsafe {
            sys::ma_decoder_init_memory(
                data.as_ptr() as *const _,
                data.len() as _,
                config.map(|c| &c.0 as *const _).unwrap_or(std::ptr::null()),
                Arc::deref(&decoder).as_ptr() as *const _ as *mut _,
            )
        };

        map_result!(
            result,
            SyncDecoder {
                inner: unsafe { std::mem::transmute(decoder) }
            }
        )
    }

    pub fn from_reader(
        reader: Box<dyn SeekRead>,
        config: Option<&DecoderConfig>,
    ) -> Result<Self, Error> {
        let decoder = Arc::new(SpinRwLock::new(MaybeUninit::<RawDecoder>::uninit()));

        let user_data = Box::new(reader);

        let result = unsafe {
            sys::ma_decoder_init(
                Some(decoder_read_with_reader),
                Some(decoder_seek_with_reader),
                Box::into_raw(user_data) as *mut _,
                config.map(|c| &c.0 as *const _).unwrap_or(std::ptr::null()),
                Arc::deref(&decoder).as_ptr() as *const _ as *mut _,
            )
        };

        map_result!(
            result,
            SyncDecoder {
                inner: unsafe { std::mem::transmute(decoder) }
            }
        )
    }

    /// This will block until the lock for the inner decoder is acquired before calling
    /// `read_pcm_frames`.
    #[inline]
    pub fn read_pcm_frames(&self, output: &mut FramesMut) -> u64 {
        self.inner.write().read_pcm_frames(output)
    }

    /// This will immediately return with 0 if the inner decoder is currently locked, if it is not
    /// this will acquire the lock and return the number of frames written.
    #[inline]
    pub fn try_read_pcm_frames(&self, output: &mut FramesMut) -> u64 {
        if let Some(ref mut locked) = self.inner.try_write() {
            locked.read_pcm_frames(output)
        } else {
            0
        }
    }

    #[inline]
    pub fn seek_to_pcm_frame(&self, frame_index: u64) -> Result<(), Error> {
        self.inner.write().seek_to_pcm_frame(frame_index)
    }

    #[inline]
    pub fn length_in_pcm_frames(&self) -> u64 {
        self.inner.write().length_in_pcm_frames()
    }

    #[inline]
    pub fn output_format(&self) -> Format {
        self.inner.read().output_format()
    }

    #[inline]
    pub fn output_channels(&self) -> u32 {
        self.inner.read().output_channels()
    }

    #[inline]
    pub fn output_sample_rate(&self) -> u32 {
        self.inner.read().output_sample_rate()
    }

    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, RawDecoder> {
        self.inner.write()
    }

    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, RawDecoder> {
        self.inner.read()
    }

    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, RawDecoder>> {
        self.inner.try_write()
    }

    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, RawDecoder>> {
        self.inner.try_read()
    }
}

impl Clone for SyncDecoder {
    fn clone(&self) -> SyncDecoder {
        SyncDecoder {
            inner: Arc::clone(&self.inner),
        }
    }
}

unsafe impl Send for SyncDecoder {}
unsafe impl Sync for SyncDecoder {}

pub struct Decoder {
    inner: Box<RawDecoder>,
}

impl Decoder {
    pub fn from_file(file: &str, config: Option<&DecoderConfig>) -> Result<Self, Error> {
        let decoder = Box::new(MaybeUninit::<RawDecoder>::uninit());
        let filename = CString::new(file.to_string()).map_err(|_err| Error::InvalidFile)?;

        let result = unsafe {
            sys::ma_decoder_init_file(
                filename.as_ptr() as *const _,
                config.map(|c| &c.0 as *const _).unwrap_or(std::ptr::null()),
                &*decoder as *const _ as *mut _,
            )
        };

        map_result!(
            result,
            Decoder {
                inner: unsafe { std::mem::transmute(decoder) }
            }
        )
    }

    pub fn from_memory(data: &[u8], config: Option<&DecoderConfig>) -> Result<Self, Error> {
        let decoder = Box::new(MaybeUninit::<RawDecoder>::uninit());

        let result = unsafe {
            sys::ma_decoder_init_memory(
                data.as_ptr() as *const _,
                data.len() as _,
                config.map(|c| &c.0 as *const _).unwrap_or(std::ptr::null()),
                &*decoder as *const _ as *mut _,
            )
        };

        map_result!(
            result,
            Decoder {
                inner: unsafe { std::mem::transmute(decoder) }
            }
        )
    }

    pub fn from_reader(
        reader: Box<dyn SeekRead>,
        config: Option<&DecoderConfig>,
    ) -> Result<Self, Error> {
        let decoder = Box::new(MaybeUninit::<RawDecoder>::uninit());
        let user_data = Box::new(reader);

        let result = unsafe {
            sys::ma_decoder_init(
                Some(decoder_read_with_reader),
                Some(decoder_seek_with_reader),
                Box::into_raw(user_data) as *mut _,
                config.map(|c| &c.0 as *const _).unwrap_or(std::ptr::null()),
                decoder.as_ptr() as *mut _,
            )
        };

        map_result!(
            result,
            Decoder {
                inner: unsafe { std::mem::transmute(decoder) }
            }
        )
    }
}

pub trait SeekRead: io::Read + io::Seek {}

// Create a blanket implementation so that everything that implements both
// io::Read and io::Seek also implements SeekRead.
impl<T> SeekRead for T where T: io::Read + io::Seek {}

unsafe extern "C" fn decoder_read_with_reader(
    decoder: *mut sys::ma_decoder,
    buffer_out: *mut std::ffi::c_void,
    bytes_to_read: usize,
) -> usize {
    let reader: &mut Box<dyn SeekRead> = std::mem::transmute((*decoder).pUserData);
    let buffer = std::slice::from_raw_parts_mut(buffer_out as _, bytes_to_read);
    
    // FIXME: unwinding from Rust code into foreign code results in undefined behavior so
    //        we might have to use a "poisoning" scheme the same way it is implemented
    //        in device_io. I'm not sure if the performance impact is worth it though.
    reader.read(buffer).expect("failed to read in decoder")
}

unsafe extern "C" fn decoder_seek_with_reader(
    decoder: *mut sys::ma_decoder,
    byte_offset: std::os::raw::c_int,
    origin: sys::ma_seek_origin,
) -> sys::ma_bool32 {
    let reader: &mut Box<dyn SeekRead> = std::mem::transmute((*decoder).pUserData);
    let pos = match origin {
        sys::ma_seek_origin_start => io::SeekFrom::Start(byte_offset as _),
        sys::ma_seek_origin_current => io::SeekFrom::Current(byte_offset as _),
        sys::ma_seek_origin_end => io::SeekFrom::End(byte_offset as _),
        
        // FIXME: see fixme in decoder_read_with_reader
        _ => unreachable!("unknown seek origin"),
    };
    
    // FIXME: see fixme in decoder_read_with_reader
    reader.seek(pos).expect("failed to seek in decoder") as _
}

impl Deref for Decoder {
    type Target = RawDecoder;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Decoder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

unsafe impl Send for Decoder {}
unsafe impl Sync for Decoder {}
