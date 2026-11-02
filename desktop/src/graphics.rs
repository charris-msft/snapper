use anyhow::{ensure, Result};
use eframe::{egui_wgpu, wgpu};
use serde_json::{json, Value};
use std::sync::Arc;

fn adapter_rank(kind: wgpu::DeviceType) -> u8 {
    match kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

fn instance_descriptor() -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        flags: wgpu::InstanceFlags::from_build_config(),
        backend_options: wgpu::BackendOptions {
            dx12: wgpu::Dx12BackendOptions {
                shader_compiler: wgpu::Dx12Compiler::Fxc,
            },
            ..Default::default()
        },
    }
}

pub fn configuration() -> egui_wgpu::WgpuConfiguration {
    let setup = egui_wgpu::WgpuSetupCreateNew {
        instance_descriptor: instance_descriptor(),
        native_adapter_selector: Some(Arc::new(|adapters, surface| {
            adapters.iter()
                .filter(|adapter|adapter.get_info().backend==wgpu::Backend::Dx12
                    &&surface.is_none_or(|surface|adapter.is_surface_supported(surface)))
                .min_by_key(|adapter|adapter_rank(adapter.get_info().device_type))
                .cloned()
                .ok_or_else(||"No compatible Direct3D 12 adapter or Windows WARP software renderer can present this window. Update the display driver or use a Windows session with D3D12/WARP support; no graphics/security setting was changed.".into())
        })),
        trace_path: None,
        ..Default::default()
    };
    egui_wgpu::WgpuConfiguration {
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: Some(2),
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

pub fn adapter_report(adapter: &wgpu::Adapter) -> Value {
    let info = adapter.get_info();
    json!({"backend":format!("{:?}",info.backend),"name":info.name,
        "device_type":format!("{:?}",info.device_type),"driver":info.driver,
        "software_fallback":info.device_type==wgpu::DeviceType::Cpu,
        "max_texture_dimension_2d":adapter.limits().max_texture_dimension_2d})
}

pub fn diagnostics() -> Result<Value> {
    ensure!(wgpu::Instance::enabled_backend_features().contains(wgpu::Backends::DX12),
        "This executable was built without the required wgpu DX12 backend; rebuild with the pinned dx12 feature.");
    let instance = wgpu::Instance::new(&instance_descriptor());
    let adapters = instance.enumerate_adapters(wgpu::Backends::DX12);
    ensure!(
        !adapters.is_empty(),
        "No Direct3D 12 adapters, including WARP, were enumerated."
    );
    Ok(
        json!({"adapters":adapters.iter().map(adapter_report).collect::<Vec<_>>(),
        "policy":"Compatible D3D12 hardware preferred, CPU/WARP last; FXC uses Windows' shader compiler.",
        "native_window_rendered":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_backend_never_depends_on_remote_opengl() {
        assert!(wgpu::Instance::enabled_backend_features().contains(wgpu::Backends::DX12));
        let config = configuration();
        let egui_wgpu::WgpuSetup::CreateNew(setup) = config.wgpu_setup else {
            panic!("Unexpected renderer setup")
        };
        assert_eq!(setup.instance_descriptor.backends, wgpu::Backends::DX12);
        assert!(matches!(
            setup
                .instance_descriptor
                .backend_options
                .dx12
                .shader_compiler,
            wgpu::Dx12Compiler::Fxc
        ));
        assert!(setup.native_adapter_selector.is_some());
        assert!(setup.trace_path.is_none());
    }
    #[test]
    fn software_is_an_explicit_last_resort_not_preferred_over_hardware() {
        assert!(adapter_rank(wgpu::DeviceType::Cpu) > adapter_rank(wgpu::DeviceType::VirtualGpu));
        assert!(
            adapter_rank(wgpu::DeviceType::Cpu) > adapter_rank(wgpu::DeviceType::IntegratedGpu)
        );
    }
}
