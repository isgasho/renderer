// TODO: pub(crate) should disappear?
pub mod alloc;
mod commands;
mod device;
mod entry;
mod gltf_mesh;
mod helpers;
mod instance;
mod swapchain;

use super::ecs::components::{GltfMesh, GltfMeshBufferIndex};
use ash::{version::DeviceV1_0, vk};
use cgmath;
use imgui;
use specs::prelude::*;
use std::{
    cmp::min,
    mem::{size_of, transmute},
    os::raw::c_uchar,
    path::PathBuf,
    ptr,
    slice::{from_raw_parts, from_raw_parts_mut},
    sync::Arc,
    thread, u64,
};
use winit;

use self::{
    commands::{CommandBuffer, CommandPool},
    device::Device,
    helpers::*,
    instance::Instance,
};

pub use self::gltf_mesh::load as load_gltf;
pub use self::helpers::{new_buffer, Buffer};

// TODO: rename
pub struct RenderFrame {
    pub instance: Arc<Instance>,
    pub device: Arc<Device>,
    pub swapchain: Arc<Swapchain>,
    pub framebuffer: Arc<Framebuffer>,
    pub image_index: u32,
    pub present_semaphore: Arc<Semaphore>,
    pub rendering_complete_semaphore: Arc<Semaphore>,
    pub graphics_command_pool: Arc<CommandPool>,
    pub compute_command_pool: Arc<CommandPool>,
    pub descriptor_pool: Arc<DescriptorPool>,
    pub renderpass: Arc<RenderPass>,
    pub depth_pipeline: Arc<Pipeline>,
    pub depth_pipeline_layout: Arc<PipelineLayout>,
    pub gltf_pipeline: Arc<Pipeline>,
    pub gltf_pipeline_layout: Arc<PipelineLayout>,
    pub ubo_set: Arc<DescriptorSet>,
    pub ubo_buffer: Arc<Buffer>,
    pub model_set: Arc<DescriptorSet>,
    pub model_buffer: Arc<Buffer>,
    pub culled_commands_buffer: Arc<Buffer>,
    pub culled_index_buffer: Option<Arc<Buffer>>,
    pub cull_pipeline: Arc<Pipeline>,
    pub cull_pipeline_layout: Arc<PipelineLayout>,
    pub cull_set_layout: Arc<DescriptorSetLayout>,
    pub cull_set: Arc<DescriptorSet>,
    pub cull_complete_semaphore: Arc<Semaphore>,
}

impl RenderFrame {
    pub fn new() -> (RenderFrame, winit::EventsLoop) {
        let (instance, events_loop) = Instance::new(1920, 1080).expect("Failed to create instance");
        let instance = Arc::new(instance);
        let device = Arc::new(Device::new(&instance).expect("Failed to create device"));
        device.set_object_name(
            vk::ObjectType::DEVICE,
            unsafe { transmute(device.handle()) },
            "Device",
        );
        let swapchain = new_swapchain(&instance, &device);
        let present_semaphore = new_semaphore(Arc::clone(&device));
        let cull_complete_semaphore = new_semaphore(Arc::clone(&device));
        let rendering_complete_semaphore = new_semaphore(Arc::clone(&device));
        let graphics_command_pool = CommandPool::new(
            Arc::clone(&device),
            device.graphics_queue_family,
            vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
        );
        let compute_command_pool = CommandPool::new(
            Arc::clone(&device),
            device.compute_queue_family,
            vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
        );
        let main_renderpass = RenderFrame::setup_renderpass(Arc::clone(&device), &swapchain);
        let framebuffer =
            setup_framebuffer(&instance, Arc::clone(&device), &swapchain, &main_renderpass);

        let descriptor_pool = new_descriptor_pool(
            Arc::clone(&device),
            30,
            &[
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: 4096,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: 4096,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    descriptor_count: 512,
                },
            ],
        );

        let command_generation_descriptor_set_layout = new_descriptor_set_layout(
            Arc::clone(&device),
            &[
                vk::DescriptorSetLayoutBinding {
                    binding: 0,
                    descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::COMPUTE,
                    p_immutable_samplers: ptr::null(),
                },
                vk::DescriptorSetLayoutBinding {
                    binding: 1,
                    descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::COMPUTE,
                    p_immutable_samplers: ptr::null(),
                },
                vk::DescriptorSetLayoutBinding {
                    binding: 2,
                    descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::COMPUTE,
                    p_immutable_samplers: ptr::null(),
                },
                vk::DescriptorSetLayoutBinding {
                    binding: 3,
                    descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::COMPUTE,
                    p_immutable_samplers: ptr::null(),
                },
            ],
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET_LAYOUT,
            unsafe { transmute::<_, u64>(command_generation_descriptor_set_layout.handle) },
            "Command Generation Descriptor Set Layout",
        );
        let ubo_set_layout = new_descriptor_set_layout(
            Arc::clone(&device),
            &[vk::DescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::COMPUTE,
                p_immutable_samplers: ptr::null(),
            }],
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET_LAYOUT,
            unsafe { transmute::<_, u64>(ubo_set_layout.handle) },
            "UBO Set Layout",
        );
        let model_view_set_layout = new_descriptor_set_layout(
            Arc::clone(&device),
            &[vk::DescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                p_immutable_samplers: ptr::null(),
            }],
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET_LAYOUT,
            unsafe { transmute::<_, u64>(model_view_set_layout.handle) },
            "Model View Set Layout",
        );

        let model_set_layout = new_descriptor_set_layout(
            Arc::clone(&device),
            &[vk::DescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::VERTEX,
                p_immutable_samplers: ptr::null(),
            }],
        );

        #[repr(C)]
        struct MeshData {
            entity_id: u32,
            gltf_id: u32,
            index_count: u32,
            index_offset: u32,
            vertex_offset: u32,
        }

        let command_generation_pipeline_layout = new_pipeline_layout(
            Arc::clone(&device),
            &[
                ubo_set_layout.handle,
                command_generation_descriptor_set_layout.handle,
            ],
            &[vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                offset: 0,
                size: size_of::<MeshData>() as u32,
            }],
        );
        let command_generation_pipeline = new_compute_pipeline(
            Arc::clone(&device),
            &command_generation_pipeline_layout,
            &PathBuf::from(env!("OUT_DIR")).join("generate_work.comp.spv"),
        );

        let command_generation_descriptor_set = new_descriptor_set(
            Arc::clone(&device),
            Arc::clone(&descriptor_pool),
            &command_generation_descriptor_set_layout,
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET,
            unsafe { transmute::<_, u64>(command_generation_descriptor_set.handle) },
            "Command Generation Descriptor Set",
        );
        let command_generation_buffer = new_buffer(
            Arc::clone(&device),
            vk::BufferUsageFlags::INDIRECT_BUFFER
                | vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::TRANSFER_DST,
            alloc::VmaAllocationCreateFlagBits(0),
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            size_of::<u32>() as vk::DeviceSize * 5 * 2400,
        );
        device.set_object_name(
            vk::ObjectType::BUFFER,
            unsafe { transmute::<_, u64>(command_generation_buffer.handle) },
            "indirect draw commands buffer",
        );

        let gltf_pipeline_layout = new_pipeline_layout(
            Arc::clone(&device),
            &[ubo_set_layout.handle, model_set_layout.handle],
            &[],
        );
        device.set_object_name(
            vk::ObjectType::PIPELINE_LAYOUT,
            unsafe { transmute::<_, u64>(gltf_pipeline_layout.handle) },
            "GLTF Pipeline Layout",
        );
        let gltf_pipeline = {
            let input_attributes = &[
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 1,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 0,
                },
            ];
            let input_bindings = &[
                vk::VertexInputBindingDescription {
                    binding: 0,
                    stride: size_of::<f32>() as u32 * 3,
                    input_rate: vk::VertexInputRate::VERTEX,
                },
                vk::VertexInputBindingDescription {
                    binding: 1,
                    stride: size_of::<f32>() as u32 * 3,
                    input_rate: vk::VertexInputRate::VERTEX,
                },
            ];
            let viewports = [vk::Viewport {
                x: 0.0,
                y: (instance.window_height as f32),
                width: instance.window_width as f32,
                height: -(instance.window_height as f32),
                min_depth: 0.0,
                max_depth: 1.0,
            }];
            let scissors = [vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: instance.window_width,
                    height: instance.window_height,
                },
            }];
            let noop_stencil_state = vk::StencilOpState {
                fail_op: vk::StencilOp::KEEP,
                pass_op: vk::StencilOp::KEEP,
                depth_fail_op: vk::StencilOp::KEEP,
                compare_op: vk::CompareOp::ALWAYS,
                compare_mask: 0,
                write_mask: 0,
                reference: 0,
            };
            let color_blend_attachment_states = [vk::PipelineColorBlendAttachmentState {
                blend_enable: 1,
                src_color_blend_factor: vk::BlendFactor::SRC_ALPHA,
                dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                color_blend_op: vk::BlendOp::ADD,
                src_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                dst_alpha_blend_factor: vk::BlendFactor::ZERO,
                alpha_blend_op: vk::BlendOp::ADD,
                color_write_mask: vk::ColorComponentFlags::all(),
            }];
            new_graphics_pipeline2(
                Arc::clone(&device),
                &[
                    (
                        vk::ShaderStageFlags::VERTEX,
                        PathBuf::from(env!("OUT_DIR")).join("gltf_mesh.vert.spv"),
                    ),
                    (
                        vk::ShaderStageFlags::FRAGMENT,
                        PathBuf::from(env!("OUT_DIR")).join("gltf_mesh.frag.spv"),
                    ),
                ],
                vk::GraphicsPipelineCreateInfo {
                    p_vertex_input_state: &vk::PipelineVertexInputStateCreateInfo {
                        vertex_attribute_description_count: input_attributes.len() as u32,
                        p_vertex_attribute_descriptions: input_attributes.as_ptr(),
                        vertex_binding_description_count: input_bindings.len() as u32,
                        p_vertex_binding_descriptions: input_bindings.as_ptr(),
                        ..Default::default()
                    },
                    p_input_assembly_state: &vk::PipelineInputAssemblyStateCreateInfo {
                        topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                        ..Default::default()
                    },
                    p_viewport_state: &vk::PipelineViewportStateCreateInfo {
                        scissor_count: scissors.len() as u32,
                        p_scissors: scissors.as_ptr(),
                        viewport_count: viewports.len() as u32,
                        p_viewports: viewports.as_ptr(),
                        ..Default::default()
                    },
                    p_rasterization_state: &vk::PipelineRasterizationStateCreateInfo {
                        cull_mode: vk::CullModeFlags::BACK,
                        front_face: vk::FrontFace::CLOCKWISE,
                        line_width: 1.0,
                        polygon_mode: vk::PolygonMode::FILL,
                        ..Default::default()
                    },
                    p_multisample_state: &vk::PipelineMultisampleStateCreateInfo {
                        rasterization_samples: vk::SampleCountFlags::TYPE_1,
                        ..Default::default()
                    },
                    p_depth_stencil_state: &vk::PipelineDepthStencilStateCreateInfo {
                        depth_test_enable: 1,
                        depth_compare_op: vk::CompareOp::LESS_OR_EQUAL,
                        depth_bounds_test_enable: 1,
                        stencil_test_enable: 0,
                        front: noop_stencil_state,
                        back: noop_stencil_state,
                        max_depth_bounds: 1.0,
                        min_depth_bounds: 0.0,
                        ..Default::default()
                    },
                    p_color_blend_state: &vk::PipelineColorBlendStateCreateInfo {
                        attachment_count: color_blend_attachment_states.len() as u32,
                        p_attachments: color_blend_attachment_states.as_ptr(),
                        ..Default::default()
                    },
                    layout: gltf_pipeline_layout.handle,
                    render_pass: main_renderpass.handle,
                    subpass: 1,
                    ..Default::default()
                },
            )
        };
        device.set_object_name(
            vk::ObjectType::PIPELINE,
            unsafe { transmute::<_, u64>(gltf_pipeline.handle) },
            "GLTF Pipeline",
        );
        let depth_pipeline_layout =
            new_pipeline_layout(Arc::clone(&device), &[ubo_set_layout.handle], &[]);
        let depth_pipeline = {
            let input_attributes = &[vk::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: vk::Format::R32G32B32_SFLOAT,
                offset: 0,
            }];
            let input_bindings = &[vk::VertexInputBindingDescription {
                binding: 0,
                stride: size_of::<f32>() as u32 * 3,
                input_rate: vk::VertexInputRate::VERTEX,
            }];
            let viewports = [vk::Viewport {
                x: 0.0,
                y: (instance.window_height as f32),
                width: instance.window_width as f32,
                height: -(instance.window_height as f32),
                min_depth: 0.0,
                max_depth: 1.0,
            }];
            let scissors = [vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: instance.window_width,
                    height: instance.window_height,
                },
            }];
            let noop_stencil_state = vk::StencilOpState {
                fail_op: vk::StencilOp::KEEP,
                pass_op: vk::StencilOp::KEEP,
                depth_fail_op: vk::StencilOp::KEEP,
                compare_op: vk::CompareOp::ALWAYS,
                compare_mask: 0,
                write_mask: 0,
                reference: 0,
            };
            let color_blend_attachment_states = [vk::PipelineColorBlendAttachmentState {
                blend_enable: 1,
                src_color_blend_factor: vk::BlendFactor::SRC_ALPHA,
                dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                color_blend_op: vk::BlendOp::ADD,
                src_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                dst_alpha_blend_factor: vk::BlendFactor::ZERO,
                alpha_blend_op: vk::BlendOp::ADD,
                color_write_mask: vk::ColorComponentFlags::all(),
            }];
            new_graphics_pipeline2(
                Arc::clone(&device),
                &[(
                    vk::ShaderStageFlags::VERTEX,
                    PathBuf::from(env!("OUT_DIR")).join("depth_prepass.vert.spv"),
                )],
                vk::GraphicsPipelineCreateInfo {
                    p_vertex_input_state: &vk::PipelineVertexInputStateCreateInfo {
                        vertex_attribute_description_count: input_attributes.len() as u32,
                        p_vertex_attribute_descriptions: input_attributes.as_ptr(),
                        vertex_binding_description_count: input_bindings.len() as u32,
                        p_vertex_binding_descriptions: input_bindings.as_ptr(),
                        ..Default::default()
                    },
                    p_input_assembly_state: &vk::PipelineInputAssemblyStateCreateInfo {
                        topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                        ..Default::default()
                    },
                    p_viewport_state: &vk::PipelineViewportStateCreateInfo {
                        scissor_count: scissors.len() as u32,
                        p_scissors: scissors.as_ptr(),
                        viewport_count: viewports.len() as u32,
                        p_viewports: viewports.as_ptr(),
                        ..Default::default()
                    },
                    p_rasterization_state: &vk::PipelineRasterizationStateCreateInfo {
                        cull_mode: vk::CullModeFlags::BACK,
                        front_face: vk::FrontFace::CLOCKWISE,
                        line_width: 1.0,
                        polygon_mode: vk::PolygonMode::FILL,
                        ..Default::default()
                    },
                    p_multisample_state: &vk::PipelineMultisampleStateCreateInfo {
                        rasterization_samples: vk::SampleCountFlags::TYPE_1,
                        ..Default::default()
                    },
                    p_depth_stencil_state: &vk::PipelineDepthStencilStateCreateInfo {
                        depth_test_enable: 1,
                        depth_write_enable: 1,
                        depth_compare_op: vk::CompareOp::LESS_OR_EQUAL,
                        depth_bounds_test_enable: 1,
                        stencil_test_enable: 0,
                        front: noop_stencil_state,
                        back: noop_stencil_state,
                        max_depth_bounds: 1.0,
                        min_depth_bounds: 0.0,
                        ..Default::default()
                    },
                    p_color_blend_state: &vk::PipelineColorBlendStateCreateInfo {
                        attachment_count: color_blend_attachment_states.len() as u32,
                        p_attachments: color_blend_attachment_states.as_ptr(),
                        ..Default::default()
                    },
                    layout: depth_pipeline_layout.handle,
                    render_pass: main_renderpass.handle,
                    subpass: 0,
                    ..Default::default()
                },
            )
        };

        device.set_object_name(
            vk::ObjectType::PIPELINE,
            unsafe { transmute::<_, u64>(depth_pipeline.handle) },
            "Depth Pipeline",
        );

        let ubo_set = new_descriptor_set(
            Arc::clone(&device),
            Arc::clone(&descriptor_pool),
            &ubo_set_layout,
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET,
            unsafe { transmute::<_, u64>(ubo_set.handle) },
            "UBO Set",
        );
        let ubo_buffer = new_buffer(
            Arc::clone(&device),
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            alloc::VmaAllocationCreateFlagBits::VMA_ALLOCATION_CREATE_MAPPED_BIT,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
            4 * 4 * 4 * 4096,
        );
        {
            let buffer_updates = &[vk::DescriptorBufferInfo {
                buffer: ubo_buffer.handle,
                offset: 0,
                range: 4096 * size_of::<cgmath::Matrix4<f32>>() as vk::DeviceSize,
            }];
            unsafe {
                device.device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet {
                        s_type: vk::StructureType::WRITE_DESCRIPTOR_SET,
                        p_next: ptr::null(),
                        dst_set: ubo_set.handle,
                        dst_binding: 0,
                        dst_array_element: 0,
                        descriptor_count: 1,
                        descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                        p_image_info: ptr::null(),
                        p_buffer_info: buffer_updates.as_ptr(),
                        p_texel_buffer_view: ptr::null(),
                    }],
                    &[],
                );
            }
        }
        let model_view_set = new_descriptor_set(
            Arc::clone(&device),
            Arc::clone(&descriptor_pool),
            &model_view_set_layout,
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET,
            unsafe { transmute::<_, u64>(model_view_set.handle) },
            "Model View Set",
        );
        let model_view_buffer = new_buffer(
            Arc::clone(&device),
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            alloc::VmaAllocationCreateFlagBits::VMA_ALLOCATION_CREATE_MAPPED_BIT,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
            4 * 4 * 4 * 4096,
        );
        {
            let buffer_updates = &[vk::DescriptorBufferInfo {
                buffer: model_view_buffer.handle,
                offset: 0,
                range: 4096 * size_of::<cgmath::Matrix4<f32>>() as vk::DeviceSize,
            }];
            unsafe {
                device.device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet {
                        s_type: vk::StructureType::WRITE_DESCRIPTOR_SET,
                        p_next: ptr::null(),
                        dst_set: model_view_set.handle,
                        dst_binding: 0,
                        dst_array_element: 0,
                        descriptor_count: 1,
                        descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                        p_image_info: ptr::null(),
                        p_buffer_info: buffer_updates.as_ptr(),
                        p_texel_buffer_view: ptr::null(),
                    }],
                    &[],
                );
            }
        }

        let model_set = new_descriptor_set(
            Arc::clone(&device),
            Arc::clone(&descriptor_pool),
            &model_set_layout,
        );
        device.set_object_name(
            vk::ObjectType::DESCRIPTOR_SET,
            unsafe { transmute::<_, u64>(model_set.handle) },
            "Model Set",
        );
        let model_buffer = new_buffer(
            Arc::clone(&device),
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            alloc::VmaAllocationCreateFlagBits::VMA_ALLOCATION_CREATE_MAPPED_BIT,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
            4 * 4 * 4 * 4096,
        );
        {
            let buffer_updates = &[vk::DescriptorBufferInfo {
                buffer: model_buffer.handle,
                offset: 0,
                range: 4096 * size_of::<cgmath::Matrix4<f32>>() as vk::DeviceSize,
            }];
            unsafe {
                device.device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet {
                        s_type: vk::StructureType::WRITE_DESCRIPTOR_SET,
                        p_next: ptr::null(),
                        dst_set: model_set.handle,
                        dst_binding: 0,
                        dst_array_element: 0,
                        descriptor_count: 1,
                        descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                        p_image_info: ptr::null(),
                        p_buffer_info: buffer_updates.as_ptr(),
                        p_texel_buffer_view: ptr::null(),
                    }],
                    &[],
                );
            }
        }

        (
            RenderFrame {
                instance: Arc::clone(&instance),
                device: Arc::clone(&device),
                framebuffer: Arc::clone(&framebuffer),
                image_index: 0,
                depth_pipeline: Arc::new(depth_pipeline),
                depth_pipeline_layout: Arc::new(depth_pipeline_layout),
                gltf_pipeline: Arc::new(gltf_pipeline),
                gltf_pipeline_layout: Arc::new(gltf_pipeline_layout),
                graphics_command_pool,
                compute_command_pool,
                descriptor_pool,
                ubo_set: Arc::new(ubo_set),
                ubo_buffer,
                model_set: Arc::new(model_set),
                model_buffer,
                present_semaphore: Arc::clone(&present_semaphore),
                rendering_complete_semaphore: Arc::clone(&rendering_complete_semaphore),
                renderpass: Arc::clone(&main_renderpass),
                swapchain: Arc::clone(&swapchain),
                culled_commands_buffer: Arc::clone(&command_generation_buffer),
                culled_index_buffer: None,
                cull_pipeline: Arc::new(command_generation_pipeline),
                cull_pipeline_layout: Arc::new(command_generation_pipeline_layout),
                cull_set_layout: Arc::new(command_generation_descriptor_set_layout),
                cull_set: Arc::new(command_generation_descriptor_set),
                cull_complete_semaphore,
            },
            events_loop,
        )
    }

    fn setup_renderpass(device: Arc<Device>, swapchain: &Swapchain) -> Arc<RenderPass> {
        let attachment_descriptions = [
            vk::AttachmentDescription {
                format: swapchain.surface_format.format,
                flags: vk::AttachmentDescriptionFlags::empty(),
                samples: vk::SampleCountFlags::TYPE_1,
                load_op: vk::AttachmentLoadOp::CLEAR,
                store_op: vk::AttachmentStoreOp::STORE,
                stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
                stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
                initial_layout: vk::ImageLayout::UNDEFINED,
                final_layout: vk::ImageLayout::PRESENT_SRC_KHR,
            },
            vk::AttachmentDescription {
                format: vk::Format::D16_UNORM,
                flags: vk::AttachmentDescriptionFlags::empty(),
                samples: vk::SampleCountFlags::TYPE_1,
                load_op: vk::AttachmentLoadOp::CLEAR,
                store_op: vk::AttachmentStoreOp::DONT_CARE,
                stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
                stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
                initial_layout: vk::ImageLayout::UNDEFINED,
                final_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            },
        ];
        let color_attachment = vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        };
        let depth_attachment = vk::AttachmentReference {
            attachment: 1,
            layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        };
        let subpass_descs = [
            vk::SubpassDescription {
                color_attachment_count: 0,
                p_color_attachments: ptr::null(),
                p_depth_stencil_attachment: &depth_attachment,
                flags: Default::default(),
                pipeline_bind_point: vk::PipelineBindPoint::GRAPHICS,
                input_attachment_count: 0,
                p_input_attachments: ptr::null(),
                p_resolve_attachments: ptr::null(),
                preserve_attachment_count: 0,
                p_preserve_attachments: ptr::null(),
            },
            vk::SubpassDescription {
                color_attachment_count: 1,
                p_color_attachments: &color_attachment,
                p_depth_stencil_attachment: &depth_attachment,
                flags: Default::default(),
                pipeline_bind_point: vk::PipelineBindPoint::GRAPHICS,
                input_attachment_count: 0,
                p_input_attachments: ptr::null(),
                p_resolve_attachments: ptr::null(),
                preserve_attachment_count: 0,
                p_preserve_attachments: ptr::null(),
            },
        ];
        let subpass_dependencies = [
            vk::SubpassDependency {
                dependency_flags: Default::default(),
                src_subpass: vk::SUBPASS_EXTERNAL,
                dst_subpass: 0,
                src_stage_mask: vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                src_access_mask: Default::default(),
                dst_access_mask: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                dst_stage_mask: vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            },
            vk::SubpassDependency {
                dependency_flags: Default::default(),
                src_subpass: 0,
                dst_subpass: 1,
                src_stage_mask: vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                src_access_mask: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                dst_stage_mask: vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
                dst_access_mask: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ,
            },
        ];
        new_renderpass(
            device,
            &attachment_descriptions,
            &subpass_descs,
            &subpass_dependencies,
        )
    }
}

pub struct AcquireFramebuffer;

impl<'a> System<'a> for AcquireFramebuffer {
    type SystemData = (WriteExpect<'a, RenderFrame>, Read<'a, PresentData>);

    fn run(&mut self, (mut renderer, present_data): Self::SystemData) {
        if let Some(ref fence) = present_data.render_complete_fence {
            unsafe {
                renderer
                    .device
                    .wait_for_fences(&[fence.handle], true, u64::MAX)
                    .expect("Wait for fence failed.");
            }
        }
        renderer.image_index = unsafe {
            renderer
                .swapchain
                .handle
                .ext
                .acquire_next_image_khr(
                    renderer.swapchain.handle.swapchain,
                    u64::MAX,
                    renderer.present_semaphore.handle,
                    vk::Fence::null(),
                ).unwrap()
        };
    }
}

pub struct CullGeometry {
    semaphores: Vec<Arc<Semaphore>>,
}

impl CullGeometry {
    pub fn new(device: &Arc<Device>) -> CullGeometry {
        CullGeometry {
            semaphores: (0..MAX_PARALLEL)
                .map(|_| new_semaphore(device.clone()))
                .collect(),
        }
    }
}

static MAX_PARALLEL: usize = 4;

impl<'a> System<'a> for CullGeometry {
    type SystemData = (
        Entities<'a>,
        ReadExpect<'a, RenderFrame>,
        ReadStorage<'a, GltfMesh>,
        ReadStorage<'a, GltfMeshBufferIndex>,
    );

    fn run(&mut self, (entities, renderer, meshes, mesh_indices): Self::SystemData) {
        use std::cmp::max;
        let mut index_offset = 0;
        let total = mesh_indices.join().count();
        let parallel = max(1, min(total / 600, MAX_PARALLEL));
        for ix in 0..parallel {
            let cull_cb = commands::record_one_time(Arc::clone(&renderer.compute_command_pool), {
                |command_buffer| unsafe {
                    renderer.device.debug_marker_around(
                        command_buffer,
                        "cull pass",
                        [0.0, 1.0, 0.0, 1.0],
                        || {
                            renderer.device.cmd_bind_descriptor_sets(
                                command_buffer,
                                vk::PipelineBindPoint::COMPUTE,
                                renderer.cull_pipeline_layout.handle,
                                0,
                                &[renderer.ubo_set.handle, renderer.cull_set.handle],
                                &[],
                            );
                            renderer.device.cmd_bind_pipeline(
                                command_buffer,
                                vk::PipelineBindPoint::COMPUTE,
                                renderer.cull_pipeline.handle,
                            );
                            for (entity, mesh, mesh_index) in (&*entities, &meshes, &mesh_indices)
                                .join()
                                .skip(total / parallel * ix)
                                .take(total / parallel)
                            {
                                let constants = [
                                    entity.id() as u32,
                                    mesh_index.0,
                                    mesh.index_len as u32,
                                    index_offset,
                                    0,
                                ];
                                index_offset += mesh.index_len as u32;

                                let casted: &[u8] = {
                                    from_raw_parts(
                                        constants.as_ptr() as *const u8,
                                        constants.len() * 4,
                                    )
                                };
                                renderer.device.cmd_push_constants(
                                    command_buffer,
                                    renderer.cull_pipeline_layout.handle,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    casted,
                                );
                                let index_len = mesh.index_len as u32;
                                let workgroup_size = 512; // TODO: make a specialization constant, not hardcoded
                                let workgroup_count = index_len / 3 / workgroup_size
                                    + min(1, index_len / 3 % workgroup_size);
                                renderer
                                    .device
                                    .cmd_dispatch(command_buffer, workgroup_count, 1, 1);
                            }
                        },
                    );
                }
            });
            let wait_semaphores = &[];
            let signal_semaphores = if parallel > 1 {
                [self.semaphores[ix].handle]
            } else {
                [renderer.cull_complete_semaphore.handle]
            };
            let dst_stage_masks = vec![vk::PipelineStageFlags::TOP_OF_PIPE; wait_semaphores.len()];
            let submits = [vk::SubmitInfo {
                s_type: vk::StructureType::SUBMIT_INFO,
                p_next: ptr::null(),
                wait_semaphore_count: wait_semaphores.len() as u32,
                p_wait_semaphores: wait_semaphores.as_ptr(),
                p_wait_dst_stage_mask: dst_stage_masks.as_ptr(),
                command_buffer_count: 1,
                p_command_buffers: &*cull_cb,
                signal_semaphore_count: signal_semaphores.len() as u32,
                p_signal_semaphores: signal_semaphores.as_ptr(),
            }];
            let submit_fence = new_fence(Arc::clone(&renderer.device));
            renderer.device.set_object_name(
                vk::ObjectType::FENCE,
                unsafe { transmute::<_, u64>(submit_fence.handle) },
                &format!("cull async compute phase {} submit fence", ix),
            );

            let queue = renderer.device.compute_queues[ix].lock();

            unsafe {
                renderer
                    .device
                    .queue_submit(*queue, &submits, submit_fence.handle)
                    .unwrap();
            }

            {
                let device = Arc::clone(&renderer.device);
                thread::spawn(move || unsafe {
                    device
                        .wait_for_fences(&[submit_fence.handle], true, u64::MAX)
                        .expect("Wait for fence failed.");
                    drop(cull_cb);
                    drop(submit_fence);
                });
            }
        }

        // When async cull was sharded between queues, define a sync point
        if parallel > 1 {
            let cull_cb_integrate =
                commands::record_one_time(Arc::clone(&renderer.compute_command_pool), { |_| {} });
            let wait_semaphores = self
                .semaphores
                .iter()
                .take(parallel)
                .map(|sem| sem.handle)
                .collect::<Vec<_>>();
            let signal_semaphores = &[renderer.cull_complete_semaphore.handle];
            let dst_stage_masks = vec![vk::PipelineStageFlags::TOP_OF_PIPE; wait_semaphores.len()];
            let submits = [vk::SubmitInfo {
                s_type: vk::StructureType::SUBMIT_INFO,
                p_next: ptr::null(),
                wait_semaphore_count: wait_semaphores.len() as u32,
                p_wait_semaphores: wait_semaphores.as_ptr(),
                p_wait_dst_stage_mask: dst_stage_masks.as_ptr(),
                command_buffer_count: 1,
                p_command_buffers: &*cull_cb_integrate,
                signal_semaphore_count: signal_semaphores.len() as u32,
                p_signal_semaphores: signal_semaphores.as_ptr(),
            }];
            let submit_fence = new_fence(Arc::clone(&renderer.device));
            renderer.device.set_object_name(
                vk::ObjectType::FENCE,
                unsafe { transmute::<_, u64>(submit_fence.handle) },
                "cull async compute integration phase submit fence",
            );

            let queue = renderer.device.compute_queues[0].lock();

            unsafe {
                renderer
                    .device
                    .queue_submit(*queue, &submits, submit_fence.handle)
                    .unwrap();
            }

            {
                let device = Arc::clone(&renderer.device);
                thread::spawn(move || unsafe {
                    device
                        .device
                        .wait_for_fences(&[submit_fence.handle], true, u64::MAX)
                        .expect("Wait for fence failed.");
                    drop(cull_cb_integrate);
                    drop(submit_fence);
                });
            }
        }
    }
}

pub struct Renderer;

impl<'a> System<'a> for Renderer {
    type SystemData = (
        Entities<'a>,
        ReadExpect<'a, RenderFrame>,
        WriteExpect<'a, Gui>,
        ReadStorage<'a, GltfMesh>,
        ReadStorage<'a, GltfMeshBufferIndex>,
        Write<'a, PresentData>,
    );

    fn run(
        &mut self,
        (entities, renderer, mut gui, meshes, coarse_culled, mut present_data): Self::SystemData,
    ) {
        let total = coarse_culled.join().count() as u32;
        let command_buffer =
            commands::record_one_time(Arc::clone(&renderer.graphics_command_pool), {
                let image_index = renderer.image_index;
                let main_renderpass = Arc::clone(&renderer.renderpass);
                let framebuffer = Arc::clone(&renderer.framebuffer);
                let instance = Arc::clone(&renderer.instance);
                let device = Arc::clone(&renderer.device);
                let ubo_set = Arc::clone(&renderer.ubo_set);
                let model_set = Arc::clone(&renderer.model_set);
                let depth_pipeline = Arc::clone(&renderer.depth_pipeline);
                let depth_pipeline_layout = Arc::clone(&renderer.depth_pipeline_layout);
                let gltf_pipeline = Arc::clone(&renderer.gltf_pipeline);
                let gltf_pipeline_layout = Arc::clone(&renderer.gltf_pipeline_layout);
                let culled_index_buffer =
                    Arc::clone(renderer.culled_index_buffer.as_ref().unwrap());
                let culled_commands_buffer = Arc::clone(&renderer.culled_commands_buffer);
                move |command_buffer| unsafe {
                    if !gui.transitioned {
                        device.cmd_pipeline_barrier(
                            command_buffer,
                            vk::PipelineStageFlags::TOP_OF_PIPE,
                            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                            Default::default(),
                            &[],
                            &[],
                            &[vk::ImageMemoryBarrier {
                                image: gui.texture.handle,
                                subresource_range: vk::ImageSubresourceRange {
                                    aspect_mask: vk::ImageAspectFlags::COLOR,
                                    base_mip_level: 0,
                                    level_count: 1,
                                    base_array_layer: 0,
                                    layer_count: 1,
                                },
                                old_layout: vk::ImageLayout::PREINITIALIZED,
                                new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                                ..Default::default()
                            }],
                        );
                    }
                    gui.transitioned = true;
                    let clear_values = &[
                        vk::ClearValue {
                            color: vk::ClearColorValue { float32: [0.0; 4] },
                        },
                        vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: 1.0,
                                stencil: 0,
                            },
                        },
                    ];
                    let begin_info = vk::RenderPassBeginInfo {
                        s_type: vk::StructureType::RENDER_PASS_BEGIN_INFO,
                        p_next: ptr::null(),
                        render_pass: main_renderpass.handle,
                        framebuffer: framebuffer.handles[image_index as usize],
                        render_area: vk::Rect2D {
                            offset: vk::Offset2D { x: 0, y: 0 },
                            extent: vk::Extent2D {
                                width: instance.window_width,
                                height: instance.window_height,
                            },
                        },
                        clear_value_count: clear_values.len() as u32,
                        p_clear_values: clear_values.as_ptr(),
                    };

                    device.debug_marker_around(
                        command_buffer,
                        "main renderpass",
                        [0.0, 0.0, 1.0, 1.0],
                        || {
                            device.device.cmd_begin_render_pass(
                                command_buffer,
                                &begin_info,
                                vk::SubpassContents::INLINE,
                            );
                            device.debug_marker_around(
                                command_buffer,
                                "depth prepass",
                                [0.3, 0.3, 0.3, 1.0],
                                || {
                                    device.device.cmd_bind_pipeline(
                                        command_buffer,
                                        vk::PipelineBindPoint::GRAPHICS,
                                        depth_pipeline.handle,
                                    );
                                    device.device.cmd_bind_descriptor_sets(
                                        command_buffer,
                                        vk::PipelineBindPoint::GRAPHICS,
                                        depth_pipeline_layout.handle,
                                        0,
                                        &[ubo_set.handle],
                                        &[],
                                    );
                                    device.device.cmd_bind_index_buffer(
                                        command_buffer,
                                        culled_index_buffer.handle,
                                        0,
                                        vk::IndexType::UINT32,
                                    );
                                    let mesh = (&*entities, &meshes).join().next().unwrap().1;
                                    device.device.cmd_bind_vertex_buffers(
                                        command_buffer,
                                        0,
                                        &[mesh.vertex_buffer.handle],
                                        &[0],
                                    );
                                    device.device.cmd_draw_indexed_indirect(
                                        command_buffer,
                                        culled_commands_buffer.handle,
                                        0,
                                        total,
                                        size_of::<u32>() as u32 * 5,
                                    );
                                    device.device.cmd_next_subpass(
                                        command_buffer,
                                        vk::SubpassContents::INLINE,
                                    );
                                },
                            );
                            device.debug_marker_around(
                                command_buffer,
                                "gltf meshes",
                                [1.0, 0.0, 0.0, 1.0],
                                || {
                                    // gltf mesh
                                    device.device.cmd_bind_pipeline(
                                        command_buffer,
                                        vk::PipelineBindPoint::GRAPHICS,
                                        gltf_pipeline.handle,
                                    );
                                    device.device.cmd_bind_descriptor_sets(
                                        command_buffer,
                                        vk::PipelineBindPoint::GRAPHICS,
                                        gltf_pipeline_layout.handle,
                                        0,
                                        &[ubo_set.handle, model_set.handle],
                                        &[],
                                    );
                                    device.device.cmd_bind_index_buffer(
                                        command_buffer,
                                        culled_index_buffer.handle,
                                        0,
                                        vk::IndexType::UINT32,
                                    );
                                    let mesh = (&*entities, &meshes).join().next().unwrap().1;
                                    device.device.cmd_bind_vertex_buffers(
                                        command_buffer,
                                        0,
                                        &[mesh.vertex_buffer.handle, mesh.normal_buffer.handle],
                                        &[0, 0],
                                    );
                                    device.device.cmd_draw_indexed_indirect(
                                        command_buffer,
                                        culled_commands_buffer.handle,
                                        0,
                                        total,
                                        size_of::<u32>() as u32 * 5,
                                    );
                                },
                            );
                            device.debug_marker_around(
                                command_buffer,
                                "GUI",
                                [1.0, 1.0, 0.0, 1.0],
                                || {
                                    let vertex_slice = from_raw_parts_mut(
                                        gui.vertex_buffer.allocation_info.pMappedData
                                            as *mut imgui::ImDrawVert,
                                        4096,
                                    );
                                    let index_slice = from_raw_parts_mut(
                                        gui.index_buffer.allocation_info.pMappedData
                                            as *mut imgui::ImDrawIdx,
                                        4096,
                                    );
                                    let pipeline_layout = gui.pipeline_layout.handle;
                                    device.cmd_bind_descriptor_sets(
                                        command_buffer,
                                        vk::PipelineBindPoint::GRAPHICS,
                                        pipeline_layout,
                                        0,
                                        &[gui.descriptor_set.handle],
                                        &[],
                                    );
                                    device.cmd_bind_pipeline(
                                        command_buffer,
                                        vk::PipelineBindPoint::GRAPHICS,
                                        gui.pipeline.handle,
                                    );
                                    device.cmd_bind_vertex_buffers(
                                        command_buffer,
                                        0,
                                        &[gui.vertex_buffer.handle],
                                        &[0],
                                    );
                                    device.device.cmd_bind_index_buffer(
                                        command_buffer,
                                        gui.index_buffer.handle,
                                        0,
                                        vk::IndexType::UINT16,
                                    );
                                    let ui = gui.imgui.frame(
                                        imgui::FrameSize {
                                            logical_size: (
                                                instance.window_width as f64,
                                                instance.window_height as f64,
                                            ),
                                            hidpi_factor: 1.0,
                                        },
                                        1.0,
                                    );
                                    let mut opened = true;
                                    // ui.show_demo_window(&mut opened);
                                    let alloc_stats = alloc::stats(device.allocator);
                                    let s = format!("Alloc stats {:?}", alloc_stats.total);
                                    ui.window(im_str!("Renderer"))
                                        .size((300.0, 100.0), imgui::ImGuiCond::FirstUseEver)
                                        .build(|| {
                                            ui.progress_bar(0.6).build();
                                            ui.small_button(im_str!("ASD"));
                                            ui.columns(2, im_str!("BlaBla"), true);
                                            ui.small_button(im_str!("Left"));
                                            ui.next_column();
                                            ui.small_button(im_str!("Left"));
                                            ui.columns(1, im_str!("asd"), false);
                                            ui.text(im_str!("After"));
                                            /*
                                            ui.child_frame(im_str!("Allocator stats"), (200.0, 100.0))
                                                .show_borders(true)
                                                .always_show_vertical_scroll_bar(true)
                                                .build(|| {
                                                    ui.small_button(im_str!("ASD"));
                                                    ui.text_wrapped(im_str!("{}", s));
                                                });
                                                */
                                        });
                                    ui.render(|ui, draw_data| {
                                        let (x, y) = ui.imgui().display_size();
                                        let constants = [2.0 / x, 2.0 / y, -1.0, -1.0];

                                        let casted: &[u8] = {
                                            from_raw_parts(
                                                constants.as_ptr() as *const u8,
                                                constants.len() * 4,
                                            )
                                        };
                                        device.cmd_push_constants(
                                            command_buffer,
                                            pipeline_layout,
                                            vk::ShaderStageFlags::VERTEX,
                                            0,
                                            casted,
                                        );
                                        let mut vertex_offset = 0;
                                        let mut index_offset = 0;
                                        for draw_list in draw_data.into_iter() {
                                            index_slice[0..draw_list.idx_buffer.len()]
                                                .copy_from_slice(draw_list.idx_buffer);
                                            vertex_slice[0..draw_list.vtx_buffer.len()]
                                                .copy_from_slice(draw_list.vtx_buffer);
                                            for draw_cmd in draw_list.cmd_buffer {
                                                device.cmd_draw_indexed(
                                                    command_buffer,
                                                    draw_cmd.elem_count,
                                                    1,
                                                    index_offset,
                                                    vertex_offset,
                                                    0,
                                                );
                                                index_offset += draw_cmd.elem_count as u32;
                                            }
                                            vertex_offset += draw_list.vtx_buffer.len() as i32;
                                        }
                                        if false {
                                            return Err(3i8);
                                        }
                                        Ok(())
                                    }).expect("failed rendering ui");
                                },
                            );
                            device.device.cmd_end_render_pass(command_buffer);
                        },
                    );
                }
            });
        unsafe {
            let wait_semaphores = &[
                renderer.present_semaphore.handle,
                renderer.cull_complete_semaphore.handle,
            ];
            let signal_semaphores = &[renderer.rendering_complete_semaphore.handle];
            let dst_stage_masks = &[
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::COMPUTE_SHADER,
            ];
            let submits = [vk::SubmitInfo {
                s_type: vk::StructureType::SUBMIT_INFO,
                p_next: ptr::null(),
                wait_semaphore_count: wait_semaphores.len() as u32,
                p_wait_semaphores: wait_semaphores.as_ptr(),
                p_wait_dst_stage_mask: dst_stage_masks.as_ptr(),
                command_buffer_count: 1,
                p_command_buffers: &*command_buffer,
                signal_semaphore_count: signal_semaphores.len() as u32,
                p_signal_semaphores: signal_semaphores.as_ptr(),
            }];
            let queue = renderer.device.graphics_queue.lock();

            let submit_fence = new_fence(Arc::clone(&renderer.device));
            renderer.device.set_object_name(
                vk::ObjectType::FENCE,
                transmute::<_, u64>(submit_fence.handle),
                "frame submit fence",
            );

            renderer
                .device
                .queue_submit(*queue, &submits, submit_fence.handle)
                .unwrap();

            present_data.render_command_buffer = Some(command_buffer);
            present_data.render_complete_fence = Some(submit_fence);
        }
    }
}

pub struct PresentData {
    render_command_buffer: Option<CommandBuffer>,
    render_complete_fence: Option<Fence>,
}

impl Default for PresentData {
    fn default() -> PresentData {
        PresentData {
            render_command_buffer: None,
            render_complete_fence: None,
        }
    }
}

pub struct PresentFramebuffer;

impl<'a> System<'a> for PresentFramebuffer {
    type SystemData = ReadExpect<'a, RenderFrame>;

    fn run(&mut self, renderer: Self::SystemData) {
        {
            let wait_semaphores = &[renderer.rendering_complete_semaphore.handle];
            let present_info = vk::PresentInfoKHR {
                s_type: vk::StructureType::PRESENT_INFO_KHR,
                p_next: ptr::null(),
                wait_semaphore_count: wait_semaphores.len() as u32,
                p_wait_semaphores: wait_semaphores.as_ptr(),
                swapchain_count: 1,
                p_swapchains: &renderer.swapchain.handle.swapchain,
                p_image_indices: &renderer.image_index,
                p_results: ptr::null_mut(),
            };

            let queue = renderer.device.graphics_queue.lock();
            unsafe {
                renderer
                    .swapchain
                    .handle
                    .ext
                    .queue_present_khr(*queue, &present_info)
                    .unwrap();
            }
        }
    }
}

pub struct Gui {
    pub imgui: imgui::ImGui,
    pub vertex_buffer: Arc<Buffer>,
    pub index_buffer: Arc<Buffer>,
    pub texture: Image,
    pub sampler: Sampler,
    pub descriptor_set_layout: DescriptorSetLayout,
    pub descriptor_set: DescriptorSet,
    pub pipeline_layout: PipelineLayout,
    pub pipeline: Pipeline,
    pub transitioned: bool,
}

impl Gui {
    pub fn new(renderer: &RenderFrame) -> Gui {
        let mut imgui = imgui::ImGui::init();
        let vertex_buffer = new_buffer(
            renderer.device.clone(),
            vk::BufferUsageFlags::VERTEX_BUFFER,
            alloc::VmaAllocationCreateFlagBits::VMA_ALLOCATION_CREATE_MAPPED_BIT,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
            4096 * size_of::<imgui::ImDrawVert>() as vk::DeviceSize,
        );
        let index_buffer = new_buffer(
            renderer.device.clone(),
            vk::BufferUsageFlags::INDEX_BUFFER,
            alloc::VmaAllocationCreateFlagBits::VMA_ALLOCATION_CREATE_MAPPED_BIT,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
            4096 * size_of::<imgui::ImDrawIdx>() as vk::DeviceSize,
        );
        let texture = imgui.prepare_texture(|handle| {
            let mut texture = new_image(
                renderer.device.clone(),
                vk::Format::R8G8B8A8_UNORM,
                vk::Extent3D {
                    width: handle.width,
                    height: handle.height,
                    depth: 1,
                },
                vk::SampleCountFlags::TYPE_1,
                vk::ImageUsageFlags::SAMPLED,
                alloc::VmaAllocationCreateFlagBits::VMA_ALLOCATION_CREATE_MAPPED_BIT,
                alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
            );
            let texture_data = unsafe {
                from_raw_parts_mut(
                    texture.allocation_info.pMappedData as *mut c_uchar,
                    texture.allocation_info.size as usize,
                )
            };
            texture_data[0..handle.pixels.len()].copy_from_slice(handle.pixels);
            unsafe {
                alloc::vmaFlushAllocation(
                    renderer.device.allocator,
                    texture.allocation,
                    0,
                    vk::WHOLE_SIZE,
                );
                alloc::vmaUnmapMemory(renderer.device.allocator, texture.allocation);
            }
            texture.allocation_info.pMappedData = ptr::null_mut();
            texture
        });
        let sampler = new_sampler(
            renderer.device.clone(),
            &vk::SamplerCreateInfo {
                address_mode_u: vk::SamplerAddressMode::CLAMP_TO_EDGE,
                address_mode_v: vk::SamplerAddressMode::CLAMP_TO_EDGE,
                address_mode_w: vk::SamplerAddressMode::CLAMP_TO_EDGE,
                ..Default::default()
            },
        );

        let descriptor_set_layout = new_descriptor_set_layout(
            renderer.device.clone(),
            &[vk::DescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::FRAGMENT,
                p_immutable_samplers: ptr::null(),
            }],
        );

        let descriptor_set = new_descriptor_set(
            renderer.device.clone(),
            renderer.descriptor_pool.clone(),
            &descriptor_set_layout,
        );

        let pipeline_layout = new_pipeline_layout(
            renderer.device.clone(),
            &[descriptor_set_layout.handle],
            &[vk::PushConstantRange {
                offset: 0,
                size: 4 * size_of::<f32>() as u32,
                stage_flags: vk::ShaderStageFlags::VERTEX,
            }],
        );

        let pipeline = {
            let input_attributes = &[
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: size_of::<f32>() as u32 * 2,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R8G8B8A8_UNORM,
                    offset: size_of::<f32>() as u32 * 4,
                },
            ];
            let input_bindings = &[vk::VertexInputBindingDescription {
                binding: 0,
                stride: size_of::<imgui::ImDrawVert>() as u32,
                input_rate: vk::VertexInputRate::VERTEX,
            }];
            let viewports = [vk::Viewport {
                x: 0.0,
                y: (renderer.instance.window_height as f32),
                width: renderer.instance.window_width as f32,
                height: -(renderer.instance.window_height as f32),
                min_depth: 0.0,
                max_depth: 1.0,
            }];
            let scissors = [vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: renderer.instance.window_width,
                    height: renderer.instance.window_height,
                },
            }];
            let color_blend_attachment_states = [vk::PipelineColorBlendAttachmentState {
                blend_enable: 1,
                src_color_blend_factor: vk::BlendFactor::SRC_ALPHA,
                dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                color_blend_op: vk::BlendOp::ADD,
                src_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
                dst_alpha_blend_factor: vk::BlendFactor::ZERO,
                alpha_blend_op: vk::BlendOp::ADD,
                color_write_mask: vk::ColorComponentFlags::all(),
            }];
            new_graphics_pipeline2(
                renderer.device.clone(),
                &[
                    (
                        vk::ShaderStageFlags::VERTEX,
                        PathBuf::from(env!("OUT_DIR")).join("gui.vert.spv"),
                    ),
                    (
                        vk::ShaderStageFlags::FRAGMENT,
                        PathBuf::from(env!("OUT_DIR")).join("gui.frag.spv"),
                    ),
                ],
                vk::GraphicsPipelineCreateInfo {
                    p_vertex_input_state: &vk::PipelineVertexInputStateCreateInfo {
                        vertex_attribute_description_count: input_attributes.len() as u32,
                        p_vertex_attribute_descriptions: input_attributes.as_ptr(),
                        vertex_binding_description_count: input_bindings.len() as u32,
                        p_vertex_binding_descriptions: input_bindings.as_ptr(),
                        ..Default::default()
                    },
                    p_input_assembly_state: &vk::PipelineInputAssemblyStateCreateInfo {
                        topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                        ..Default::default()
                    },
                    p_viewport_state: &vk::PipelineViewportStateCreateInfo {
                        scissor_count: scissors.len() as u32,
                        p_scissors: scissors.as_ptr(),
                        viewport_count: viewports.len() as u32,
                        p_viewports: viewports.as_ptr(),
                        ..Default::default()
                    },
                    p_rasterization_state: &vk::PipelineRasterizationStateCreateInfo {
                        cull_mode: vk::CullModeFlags::NONE,
                        line_width: 1.0,
                        polygon_mode: vk::PolygonMode::FILL,
                        ..Default::default()
                    },
                    p_multisample_state: &vk::PipelineMultisampleStateCreateInfo {
                        rasterization_samples: vk::SampleCountFlags::TYPE_1,
                        ..Default::default()
                    },
                    p_depth_stencil_state: &vk::PipelineDepthStencilStateCreateInfo {
                        ..Default::default()
                    },
                    p_color_blend_state: &vk::PipelineColorBlendStateCreateInfo {
                        attachment_count: color_blend_attachment_states.len() as u32,
                        p_attachments: color_blend_attachment_states.as_ptr(),
                        ..Default::default()
                    },
                    layout: pipeline_layout.handle,
                    render_pass: renderer.renderpass.handle,
                    subpass: 1,
                    ..Default::default()
                },
            )
        };
        renderer.device.set_object_name(
            vk::ObjectType::PIPELINE,
            unsafe { transmute::<_, u64>(pipeline.handle) },
            "GUI Pipeline",
        );

        let create_view_info = vk::ImageViewCreateInfo {
            s_type: vk::StructureType::IMAGE_VIEW_CREATE_INFO,
            p_next: ptr::null(),
            flags: Default::default(),
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk::Format::R8G8B8A8_UNORM,
            components: vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            },
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
            image: texture.handle,
        };
        let image_view = unsafe {
            renderer
                .device
                .create_image_view(&create_view_info, None)
                .unwrap()
        };

        let image_updates = &[vk::DescriptorImageInfo {
            sampler: sampler.handle,
            image_view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        unsafe {
            renderer.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet {
                    s_type: vk::StructureType::WRITE_DESCRIPTOR_SET,
                    p_next: ptr::null(),
                    dst_set: descriptor_set.handle,
                    dst_binding: 0,
                    dst_array_element: 0,
                    descriptor_count: 1,
                    descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    p_image_info: image_updates.as_ptr(),
                    p_buffer_info: ptr::null(),
                    p_texel_buffer_view: ptr::null(),
                }],
                &[],
            );
        }

        Gui {
            imgui,
            vertex_buffer,
            index_buffer,
            texture,
            sampler,
            descriptor_set_layout,
            descriptor_set,
            pipeline_layout,
            pipeline,
            transitioned: false,
        }
    }
}
