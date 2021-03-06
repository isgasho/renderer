use crate::{
    ecs::custom::{ComponentStorage, EntitiesStorage},
    renderer::{
        alloc,
        device::{Buffer, CommandBuffer, Fence, TimelineSemaphore},
        systems::present::ImageIndex,
        GltfMesh, GraphicsCommandPool, RenderFrame,
    },
};
use ash::{
    version::DeviceV1_0,
    vk::{self, Handle},
};
use hashbrown::{hash_map::Entry, HashMap};
#[cfg(feature = "microprofile")]
use microprofile::scope;
use std::{mem::size_of, u64};

/// Describes layout of gltf mesh vertex data in a shared buffer
pub struct ConsolidatedMeshBuffers {
    /// Maps from vertex buffer handle to offset within the consolidated buffer
    /// Hopefully this is distinct enough
    pub vertex_offsets: HashMap<u64, vk::DeviceSize>,
    /// Next free vertex offset in the buffer that can be used for a new mesh
    next_vertex_offset: vk::DeviceSize,
    /// Maps from index buffer handle to offset within the consolidated buffer
    pub index_offsets: HashMap<u64, vk::DeviceSize>,
    /// Next free index offset in the buffer that can be used for a new mesh
    next_index_offset: vk::DeviceSize,
    /// Stores position data for each mesh
    pub position_buffer: Buffer,
    /// Stores normal data for each mesh
    pub normal_buffer: Buffer,
    /// Stores uv data for each mesh
    pub uv_buffer: Buffer,
    /// Stores index data for each mesh
    pub index_buffer: Buffer,
    /// If this semaphore is present, a modification to the consolidated buffer has happened
    /// and the user must synchronize with it
    pub sync_timeline: TimelineSemaphore,
    /// Holds the command buffer executed in the previous frame, to clean it up safely in the following frame
    previous_run_command_buffer: Option<CommandBuffer>,
    /// Holds the fence used to synchronize the transfer that occured in previous frame.
    sync_point_fence: Fence,
}

/// Identifies distinct GLTF meshes in components and copies them to a shared buffer
pub struct ConsolidateMeshBuffers;

// TODO: dynamic unloading of meshes
// TODO: use actual transfer queue for the transfers

impl ConsolidatedMeshBuffers {
    pub fn new(renderer: &RenderFrame) -> ConsolidatedMeshBuffers {
        let vertex_offsets = HashMap::new();
        let index_offsets = HashMap::new();

        let position_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            super::super::shaders::cull_set::bindings::vertex_buffer::SIZE,
        );
        let normal_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            super::super::shaders::cull_set::bindings::vertex_buffer::SIZE,
        );
        let uv_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            size_of::<super::super::shaders::UVBuffer>() as vk::DeviceSize,
        );
        let index_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::INDEX_BUFFER
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            super::super::shaders::cull_set::bindings::index_buffer::SIZE,
        );
        let sync_timeline = renderer.device.new_semaphore_timeline(renderer.frame_number * 16);
        renderer.device.set_object_name(
            sync_timeline.handle,
            "Consolidate mesh buffers sync timeline",
        );
        let sync_point_fence = renderer.device.new_fence();
        renderer.device.set_object_name(
            sync_point_fence.handle,
            "Consolidate vertex buffers sync point fence",
        );

        ConsolidatedMeshBuffers {
            vertex_offsets,
            next_vertex_offset: 0,
            index_offsets,
            next_index_offset: 0,
            position_buffer,
            normal_buffer,
            uv_buffer,
            index_buffer,
            sync_timeline,
            previous_run_command_buffer: None,
            sync_point_fence,
        }
    }
}

impl ConsolidateMeshBuffers {
    pub fn exec(
        renderer: &RenderFrame,
        entities: &EntitiesStorage,
        graphics_command_pool: &GraphicsCommandPool,
        meshes: &ComponentStorage<GltfMesh>,
        image_index: &ImageIndex,
        consolidated_mesh_buffers: &mut ConsolidatedMeshBuffers,
    ) {
        #[cfg(feature = "profiling")]
        microprofile::scope!("ecs", "consolidate mesh buffers");
        if consolidated_mesh_buffers
            .previous_run_command_buffer
            .is_some()
        {
            unsafe {
                renderer
                    .device
                    .wait_for_fences(
                        &[consolidated_mesh_buffers.sync_point_fence.handle],
                        true,
                        u64::MAX,
                    )
                    .expect("Wait for fence failed.");
            }
        }
        unsafe {
            renderer
                .device
                .reset_fences(&[consolidated_mesh_buffers.sync_point_fence.handle])
                .expect("failed to reset consolidate vertex buffers sync point fence");
        }

        let mut needs_transfer = false;
        let command_buffer = graphics_command_pool.0.record_one_time(
            "consolidate mesh buffers cb",
            |command_buffer| {
                for ix in (entities.mask() & meshes.mask()).iter() {
                    let mesh = meshes.get(ix).unwrap();
                    let ConsolidatedMeshBuffers {
                        ref mut next_vertex_offset,
                        ref mut next_index_offset,
                        ref position_buffer,
                        ref normal_buffer,
                        ref uv_buffer,
                        ref index_buffer,
                        ref mut vertex_offsets,
                        ref mut index_offsets,
                        ..
                    } = *consolidated_mesh_buffers;

                    if let Entry::Vacant(v) =
                        vertex_offsets.entry(mesh.vertex_buffer.handle.as_raw())
                    {
                        v.insert(*next_vertex_offset);
                        let size_3 = mesh.vertex_len * size_of::<[f32; 3]>() as vk::DeviceSize;
                        let size_2 = mesh.vertex_len * size_of::<[f32; 2]>() as vk::DeviceSize;
                        let offset_3 =
                            *next_vertex_offset * size_of::<[f32; 3]>() as vk::DeviceSize;
                        let offset_2 =
                            *next_vertex_offset * size_of::<[f32; 2]>() as vk::DeviceSize;

                        unsafe {
                            // vertex
                            renderer.device.cmd_copy_buffer(
                                command_buffer,
                                mesh.vertex_buffer.handle,
                                position_buffer.handle,
                                &[vk::BufferCopy::builder()
                                    .size(size_3)
                                    .dst_offset(offset_3)
                                    .build()],
                            );
                            // normal
                            renderer.device.cmd_copy_buffer(
                                command_buffer,
                                mesh.normal_buffer.handle,
                                normal_buffer.handle,
                                &[vk::BufferCopy::builder()
                                    .size(size_3)
                                    .dst_offset(offset_3)
                                    .build()],
                            );
                            // uv
                            renderer.device.cmd_copy_buffer(
                                command_buffer,
                                mesh.uv_buffer.handle,
                                uv_buffer.handle,
                                &[vk::BufferCopy::builder()
                                    .size(size_2)
                                    .dst_offset(offset_2)
                                    .build()],
                            );
                        }
                        *next_vertex_offset += mesh.vertex_len;
                        needs_transfer = true;
                    }

                    for (lod_index_buffer, index_len) in mesh.index_buffers.iter() {
                        if let Entry::Vacant(v) =
                            index_offsets.entry(lod_index_buffer.handle.as_raw())
                        {
                            v.insert(*next_index_offset);

                            unsafe {
                                renderer.device.cmd_copy_buffer(
                                    command_buffer,
                                    lod_index_buffer.handle,
                                    index_buffer.handle,
                                    &[vk::BufferCopy::builder()
                                        .size(index_len * size_of::<u32>() as vk::DeviceSize)
                                        .dst_offset(
                                            *next_index_offset * size_of::<u32>() as vk::DeviceSize,
                                        )
                                        .build()],
                                );
                            }
                            *next_index_offset += index_len;
                            needs_transfer = true;
                        }
                    }
                }
            },
        );

        dbg!(needs_transfer, image_index.0);
        if needs_transfer {
            let command_buffers = &[*command_buffer];
            let signal_semaphores = &[consolidated_mesh_buffers.sync_timeline.handle];
            let signal_semaphore_values = &[renderer.frame_number * 16 + 16];
            let mut wait_timeline = vk::TimelineSemaphoreSubmitInfo::builder()
                .wait_semaphore_values(signal_semaphore_values) // only needed because validation layers segfault
                .signal_semaphore_values(signal_semaphore_values);
            let submit = vk::SubmitInfo::builder()
                .push_next(&mut wait_timeline)
                .command_buffers(command_buffers)
                .signal_semaphores(signal_semaphores)
                .build();

            consolidated_mesh_buffers.previous_run_command_buffer = Some(command_buffer); // potentially destroys the previous one

            let queue = renderer.device.graphics_queue.lock();

            unsafe {
                renderer
                    .device
                    .queue_submit(
                        *queue,
                        &[submit],
                        consolidated_mesh_buffers.sync_point_fence.handle,
                    )
                    .unwrap();
            }
        } else {
            let signal_info = vk::SemaphoreSignalInfo::builder()
                .semaphore(consolidated_mesh_buffers.sync_timeline.handle)
                .value(renderer.frame_number * 16 + 16);
            (renderer.device.signal_semaphore)(renderer.device.handle(), &*signal_info);
            consolidated_mesh_buffers.previous_run_command_buffer = None; // potentially destroys the previous one
        }
    }
}
