//! CEF Render Handler
//!
//! Implements CEF's RenderHandler trait to capture off-screen rendered frames.
//! When shared_texture_enabled is set, CEF calls on_accelerated_paint with a
//! platform-specific handle:
//! - macOS: IOSurface → CVPixelBuffer (zero-copy)
//! - Windows: D3D11 shared HANDLE → BGRA pixel buffer (GPU→CPU copy)

use crate::events::{BrowserEvent, EventSender};
use cef::{
    AcceleratedPaintInfo, Browser, ImplRenderHandler, PaintElementType, Rect, RenderHandler,
    ScreenInfo, WrapRenderHandler, rc::Rc as _, wrap_render_handler,
};
#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
use io_surface::IOSurface;
use parking_lot::Mutex;
use std::sync::Arc;

pub struct RenderState {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
    #[cfg(target_os = "macos")]
    pub current_frame: Option<CVPixelBuffer>,
    #[cfg(target_os = "windows")]
    pub current_frame: Option<WindowsFrame>,
}

#[cfg(target_os = "windows")]
#[derive(Clone)]
pub struct WindowsFrame {
    pub data: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
            scale_factor: 1.0,
            #[cfg(target_os = "macos")]
            current_frame: None,
            #[cfg(target_os = "windows")]
            current_frame: None,
        }
    }
}

#[derive(Clone)]
pub struct OsrRenderHandler {
    state: Arc<Mutex<RenderState>>,
    sender: EventSender,
    #[cfg(target_os = "windows")]
    d3d_device: Arc<Mutex<Option<windows::Win32::Graphics::Direct3D11::ID3D11Device1>>>,
}

impl OsrRenderHandler {
    pub fn new(state: Arc<Mutex<RenderState>>, sender: EventSender) -> Self {
        #[cfg(target_os = "windows")]
        {
            Self {
                state,
                sender,
                d3d_device: Arc::new(Mutex::new(None)),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Self { state, sender }
        }
    }
}

wrap_render_handler! {
    pub struct RenderHandlerBuilder {
        handler: OsrRenderHandler,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let state = self.handler.state.lock();
                rect.x = 0;
                rect.y = 0;
                rect.width = state.width as i32;
                rect.height = state.height as i32;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            if let Some(info) = screen_info {
                let state = self.handler.state.lock();
                info.device_scale_factor = state.scale_factor;
                info.rect.x = 0;
                info.rect.y = 0;
                info.rect.width = state.width as i32;
                info.rect.height = state.height as i32;
                info.available_rect = info.rect.clone();
                info.depth = 32;
                info.depth_per_component = 8;
                info.is_monochrome = 0;
                return 1;
            }
            0
        }

        fn screen_point(
            &self,
            _browser: Option<&mut Browser>,
            view_x: ::std::os::raw::c_int,
            view_y: ::std::os::raw::c_int,
            screen_x: Option<&mut ::std::os::raw::c_int>,
            screen_y: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(screen_x) = screen_x {
                *screen_x = view_x;
            }
            if let Some(screen_y) = screen_y {
                *screen_y = view_y;
            }
            1
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            if type_ != PaintElementType::default() {
                return;
            }

            let Some(info) = info else {
                log::warn!("[browser::render_handler] on_accelerated_paint() no info");
                return;
            };

            #[cfg(target_os = "macos")]
            {
                let io_surface_ptr = info.shared_texture_io_surface;
                if io_surface_ptr.is_null() {
                    log::warn!("[browser::render_handler] on_accelerated_paint() null IOSurface");
                    return;
                }

                #[allow(deprecated)]
                let io_surface: IOSurface = unsafe {
                    TCFType::wrap_under_get_rule(io_surface_ptr as io_surface::IOSurfaceRef)
                };

                let pixel_buffer = match CVPixelBuffer::from_io_surface(&io_surface, None) {
                    Ok(pb) => pb,
                    Err(err) => {
                        log::error!("[browser::render_handler] on_accelerated_paint() CVPixelBuffer::from_io_surface failed: {:?}", err);
                        return;
                    }
                };

                self.handler.state.lock().current_frame = Some(pixel_buffer);
                let _ = self.handler.sender.send(BrowserEvent::FrameReady);
            }

            #[cfg(target_os = "windows")]
            {
                use windows::Win32::Foundation::HANDLE;
                use windows::Win32::Graphics::Direct3D11::*;
                use windows::Win32::Graphics::Dxgi::Common::*;

                let handle = info.shared_texture_handle;
                if handle.is_null() {
                    log::warn!("[browser::render_handler] on_accelerated_paint() null shared_texture_handle");
                    return;
                }

                let device1 = {
                    let mut guard = self.handler.d3d_device.lock();
                    if guard.is_none() {
                        match create_capture_device() {
                            Ok(dev) => *guard = Some(dev),
                            Err(e) => {
                                log::error!("[browser::render_handler] Failed to create D3D11 capture device: {}", e);
                                return;
                            }
                        }
                    }
                    guard.clone().unwrap()
                };

                let shared_texture: ID3D11Texture2D = unsafe {
                    match device1.OpenSharedResource1(HANDLE(handle as *mut _)) {
                        Ok(tex) => tex,
                        Err(e) => {
                            log::error!("[browser::render_handler] OpenSharedResource1 failed: {}", e);
                            return;
                        }
                    }
                };

                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { shared_texture.GetDesc(&mut desc) };
                let width = desc.Width;
                let height = desc.Height;

                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..Default::default()
                };

                let mut staging_opt: Option<ID3D11Texture2D> = None;
                match unsafe {
                    device1.CreateTexture2D(&staging_desc, None, Some(&mut staging_opt))
                } {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("[browser::render_handler] Failed to create staging texture: {}", e);
                        return;
                    }
                }
                let staging = match staging_opt {
                    Some(tex) => tex,
                    None => {
                        log::error!("[browser::render_handler] CreateTexture2D returned None");
                        return;
                    }
                };

                let device_ctx: ID3D11DeviceContext = match unsafe {
                    device1.GetImmediateContext()
                } {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        log::error!("[browser::render_handler] GetImmediateContext failed: {}", e);
                        return;
                    }
                };

                unsafe {
                    device_ctx.CopyResource(&staging, &shared_texture);
                }

                let mut mapped_sub = D3D11_MAPPED_SUBRESOURCE::default();
                let mapped = match unsafe {
                    device_ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped_sub))
                } {
                    Ok(()) => mapped_sub,
                    Err(e) => {
                        log::error!("[browser::render_handler] Map failed: {}", e);
                        return;
                    }
                };

                let row_pitch = mapped.RowPitch as usize;
                let mut pixel_data = vec![0u8; (width * height * 4) as usize];

                let src = mapped.pData as *const u8;
                let dst_width_bytes = (width * 4) as usize;
                for y in 0..height as usize {
                    let src_row = unsafe { src.add(y * row_pitch) };
                    let dst_row = &mut pixel_data[y * dst_width_bytes..(y + 1) * dst_width_bytes];
                    unsafe {
                        std::ptr::copy_nonoverlapping(src_row, dst_row.as_mut_ptr(), dst_width_bytes);
                    }
                }

                unsafe {
                    device_ctx.Unmap(&staging, 0);
                }

                let frame = WindowsFrame {
                    data: Arc::new(pixel_data),
                    width,
                    height,
                };

                self.handler.state.lock().current_frame = Some(frame);
                let _ = self.handler.sender.send(BrowserEvent::FrameReady);
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            _buffer: *const u8,
            _width: ::std::os::raw::c_int,
            _height: ::std::os::raw::c_int,
        ) {
            log::warn!("[browser::render_handler] on_paint() called unexpectedly (shared_texture_enabled should prevent this)");
        }
    }
}

#[cfg(target_os = "windows")]
fn create_capture_device() -> anyhow::Result<windows::Win32::Graphics::Direct3D11::ID3D11Device1> {
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::core::Interface;

    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;

    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

    unsafe {
        D3D11CreateDevice(
            Default::default(),
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            flags,
            Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }

    let device = device.ok_or_else(|| anyhow::anyhow!("D3D11CreateDevice returned None device"))?;
    let device1: ID3D11Device1 = device.cast()?;
    Ok(device1)
}

impl RenderHandlerBuilder {
    pub fn build(handler: OsrRenderHandler) -> cef::RenderHandler {
        Self::new(handler)
    }
}
