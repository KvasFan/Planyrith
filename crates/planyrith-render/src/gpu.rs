//! Headless device creation (docs/06 §11): prefers a real GPU, falls back to
//! a software adapter (lavapipe, WARP) when no hardware adapter exists.
//! `WGPU_BACKEND`, `WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF` are honored.

use crate::RenderError;

/// A device and its queue.
#[derive(Debug, Clone)]
pub struct Gpu {
    /// The device.
    pub device: wgpu::Device,
    /// Its queue.
    pub queue: wgpu::Queue,
    /// What the adapter is (for logs and reports).
    pub adapter: wgpu::AdapterInfo,
}

impl Gpu {
    /// Creates a device without a window (blocking).
    pub fn headless() -> Result<Gpu, RenderError> {
        pollster::block_on(Self::headless_async())
    }

    /// Creates a device without a window.
    pub async fn headless_async() -> Result<Gpu, RenderError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            match wgpu::util::initialize_adapter_from_env_or_default(&instance, None).await {
                Ok(adapter) => adapter,
                Err(first) => instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        force_fallback_adapter: true,
                        compatible_surface: None,
                        apply_limit_buckets: false,
                    })
                    .await
                    .map_err(|e| RenderError::NoAdapter(format!("{first}; software: {e}")))?,
            };
        let info = adapter.get_info();
        tracing::info!(
            "graphics adapter: {} ({:?}, {:?})",
            info.name,
            info.backend,
            info.device_type
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("planyrith headless"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .map_err(|e| RenderError::Device(e.to_string()))?;
        Ok(Gpu {
            device,
            queue,
            adapter: info,
        })
    }
}
