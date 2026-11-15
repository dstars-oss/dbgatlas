use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    Command = 1,
    EventWait = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptKind {
    Active = 0,
    Passive = 1,
    ExitWait = 2,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineEvent {
    pub sequence: u64,
    pub epoch: u64,
    pub kind: u32,
    pub code: u32,
    pub argument: u64,
}

#[derive(Clone, Debug)]
pub struct EngineEvents {
    pub events: Vec<EngineEvent>,
    pub output_byte_len: u64,
    pub output_error: u32,
}

#[derive(Clone, Debug)]
pub struct InterruptHandle {
    #[cfg(windows)]
    inner: std::sync::Arc<NativeControl>,
}

#[cfg(windows)]
#[derive(Debug)]
struct NativeControl(*mut dbgatlas_dbgeng_sys::DA_DbgEngControlHandle);
// NativeControl owns shared native state, not a session/COM interface. Its only engine
// call is SetInterrupt, allowed from any thread. Native close invalidates the borrowed
// control pointer under the same mutex before the owner releases COM. Cached event
// reads and final handle release are mutex-protected and perform no engine calls.
#[cfg(windows)]
unsafe impl Send for NativeControl {}
#[cfg(windows)]
unsafe impl Sync for NativeControl {}
#[cfg(windows)]
impl Drop for NativeControl {
    fn drop(&mut self) {
        unsafe {
            dbgatlas_dbgeng_sys::da_dbgeng_control_release(self.0);
        }
    }
}

impl InterruptHandle {
    /// Returns the raw SetInterrupt HRESULT. Stale epochs are rejected before touching
    /// the engine. S_OK only confirms delivery; E_PENDING on EXIT may require retry.
    pub fn interrupt(&self, epoch: u64, kind: InterruptKind) -> Result<i32, DbgEngError> {
        #[cfg(windows)]
        {
            let mut result = 0;
            status_to_result(unsafe {
                dbgatlas_dbgeng_sys::da_dbgeng_control_interrupt(
                    self.inner.0,
                    epoch,
                    kind as u32,
                    &mut result,
                )
            })?;
            Ok(result)
        }
        #[cfg(not(windows))]
        {
            let _ = (epoch, kind);
            Err(DbgEngError::UnsupportedPlatform)
        }
    }

    /// Drains cached metadata. A single consumer must persist sequence/gap records.
    pub fn read_events(&self) -> Result<EngineEvents, DbgEngError> {
        #[cfg(windows)]
        {
            use dbgatlas_dbgeng_sys as sys;
            let mut out = sys::DA_DbgEngEvents {
                struct_size: size_of::<sys::DA_DbgEngEvents>() as u32,
                output_error: 0,
                data: std::ptr::null(),
                len: 0,
                owner: std::ptr::null_mut(),
                output_byte_len: 0,
            };
            status_to_result(unsafe {
                sys::da_dbgeng_control_read_events(self.inner.0, &mut out)
            })?;
            struct Owner(*mut std::ffi::c_void);
            impl Drop for Owner {
                fn drop(&mut self) {
                    release_view(self.0);
                }
            }
            let _owner = Owner(out.owner);
            if out.len > 0 && out.data.is_null() {
                return Err(DbgEngError::NullText);
            }
            let events = if out.len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(out.data, out.len) }
            };
            Ok(EngineEvents {
                events: events
                    .iter()
                    .map(|e| EngineEvent {
                        sequence: e.sequence,
                        epoch: e.epoch,
                        kind: e.kind,
                        code: e.code,
                        argument: e.argument,
                    })
                    .collect(),
                output_byte_len: out.output_byte_len,
                output_error: out.output_error,
            })
        }
        #[cfg(not(windows))]
        {
            Err(DbgEngError::UnsupportedPlatform)
        }
    }
}

impl DbgEngSession {
    pub fn enable_interaction(&self, output_path: &Path) -> Result<InterruptHandle, DbgEngError> {
        #[cfg(windows)]
        {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let path = path_to_cstring(output_path)?;
            let mut handle = std::ptr::null_mut();
            status_to_result(unsafe {
                dbgatlas_dbgeng_sys::da_dbgeng_session_enable_interaction(
                    self.handle,
                    path.as_ptr(),
                    &mut handle,
                )
            })?;
            if handle.is_null() {
                return Err(DbgEngError::NullSessionHandle);
            }
            Ok(InterruptHandle {
                inner: std::sync::Arc::new(NativeControl(handle)),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = output_path;
            Err(DbgEngError::UnsupportedPlatform)
        }
    }

    /// The owner brackets one command or one event wait. End it before admitting a
    /// successor. The worker never publishes a session pointer to control threads.
    pub fn begin_activity(&self, kind: ActivityKind) -> Result<u64, DbgEngError> {
        #[cfg(windows)]
        {
            let mut epoch = 0;
            status_to_result(unsafe {
                dbgatlas_dbgeng_sys::da_dbgeng_session_begin_activity(
                    self.handle,
                    kind as u32,
                    &mut epoch,
                )
            })?;
            Ok(epoch)
        }
        #[cfg(not(windows))]
        {
            let _ = kind;
            Err(DbgEngError::UnsupportedPlatform)
        }
    }
    pub fn end_activity(&self, epoch: u64) -> Result<i32, DbgEngError> {
        #[cfg(windows)]
        {
            let mut result = 0;
            status_to_result(unsafe {
                dbgatlas_dbgeng_sys::da_dbgeng_session_end_activity(self.handle, epoch, &mut result)
            })?;
            Ok(result)
        }
        #[cfg(not(windows))]
        {
            let _ = epoch;
            Err(DbgEngError::UnsupportedPlatform)
        }
    }
    /// Blocking owner-thread event pump. Deliberately has no timeout parameter.
    pub fn wait_event(&self, epoch: u64) -> Result<i32, DbgEngError> {
        #[cfg(windows)]
        {
            let mut result = 0;
            status_to_result(unsafe {
                dbgatlas_dbgeng_sys::da_dbgeng_session_wait_event(self.handle, epoch, &mut result)
            })?;
            Ok(result)
        }
        #[cfg(not(windows))]
        {
            let _ = epoch;
            Err(DbgEngError::UnsupportedPlatform)
        }
    }
}
