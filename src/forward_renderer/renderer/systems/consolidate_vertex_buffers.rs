use super::super::{
    super::ecs::components::GltfMesh,
    alloc,
    device::{Buffer, Semaphore},
    RenderFrame,
};
use ash::{
    version::DeviceV1_0,
    vk::{self, Handle},
};
use hashbrown::{hash_map::Entry, HashMap};
use specs::prelude::*;
use std::{mem::size_of, sync::Arc, thread, u64};

/// Describes layout of gltf mesh vertex data in a shared buffer
pub struct ConsolidatedVertexBuffers {
    /// Maps from vertex buffer handle to offset within the consolidated buffer
    /// Hopefully this is distinct enough
    pub offsets: HashMap<u64, vk::DeviceSize>,
    /// Next free offset in the buffer that can be used for a new mesh
    next_offset: vk::DeviceSize,
    /// Stores position data for each mesh
    pub position_buffer: Buffer,
    /// Stores normal data for each mesh
    pub normal_buffer: Buffer,
    /// Stores uv data for each mesh
    pub uv_buffer: Buffer,
    /// If this semaphore is present, a modification to the consolidated buffer has happened
    /// and the user must synchronize with it
    pub sync_point: Option<Semaphore>,
}

/// Identifies distinct GLTF meshes in components and copies them to a shared buffer
pub struct ConsolidateVertexBuffers;

// TODO: dynamic unloading of meshes
// TODO: use actual transfer queue for the transfers

impl shred::SetupHandler<ConsolidatedVertexBuffers> for ConsolidatedVertexBuffers {
    fn setup(res: &mut Resources) {
        let renderer = res.fetch::<RenderFrame>();
        let offsets = HashMap::new();

        let position_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            size_of::<[f32; 3]>() as vk::DeviceSize * 10 /* distinct meshes */ * 30_000 /* unique vertices */
        );
        let normal_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            size_of::<[f32; 3]>() as vk::DeviceSize * 10 /* distinct meshes */ * 30_000 /* unique vertices */
        );
        let uv_buffer = renderer.device.new_buffer(
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
            alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
            size_of::<[f32; 2]>() as vk::DeviceSize * 10 /* distinct meshes */ * 30_000 /* unique vertices */
        );
        drop(renderer);

        res.insert(ConsolidatedVertexBuffers {
            offsets,
            next_offset: 0,
            position_buffer,
            normal_buffer,
            uv_buffer,
            sync_point: None,
        });
    }
}

impl<'a> System<'a> for ConsolidateVertexBuffers {
    #[allow(clippy::type_complexity)]
    type SystemData = (
        WriteExpect<'a, RenderFrame>,
        ReadStorage<'a, GltfMesh>,
        Write<'a, ConsolidatedVertexBuffers, ConsolidatedVertexBuffers>,
    );

    fn run(&mut self, (renderer, meshes, mut consolidate_vertex_buffers): Self::SystemData) {
        let mut needs_transfer = false;
        let command_buffer = renderer
            .graphics_command_pool
            .record_one_time(|command_buffer| {
                for mesh in (&meshes).join() {
                    let ConsolidatedVertexBuffers {
                        ref mut next_offset,
                        ref position_buffer,
                        ref normal_buffer,
                        ref uv_buffer,
                        ref mut offsets,
                        ..
                    } = *consolidate_vertex_buffers;

                    if let Entry::Vacant(v) = offsets.entry(mesh.vertex_buffer.handle.as_raw()) {
                        v.insert(*next_offset);
                        let size_3 = mesh.vertex_len * size_of::<[f32; 3]>() as vk::DeviceSize;
                        let size_2 = mesh.vertex_len * size_of::<[f32; 2]>() as vk::DeviceSize;
                        let offset_3 = *next_offset * size_of::<[f32; 3]>() as vk::DeviceSize;
                        let offset_2 = *next_offset * size_of::<[f32; 2]>() as vk::DeviceSize;

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
                        *next_offset += mesh.vertex_len;
                        needs_transfer = true;
                    }
                }
            });

        if needs_transfer {
            let semaphore = renderer.device.new_semaphore();
            renderer.device.set_object_name(
                semaphore.handle,
                "Consolidate Vertex buffers sync point semaphore",
            );
            let command_buffers = &[*command_buffer];
            let signal_semaphores = &[semaphore.handle];
            let submit = vk::SubmitInfo::builder()
                .command_buffers(command_buffers)
                .signal_semaphores(signal_semaphores)
                .build();

            let submit_fence = renderer.device.new_fence();
            renderer.device.set_object_name(
                submit_fence.handle,
                "Consolidate Vertex buffers submit fence",
            );

            consolidate_vertex_buffers.sync_point = Some(semaphore);

            let queue = renderer.device.graphics_queue.lock();

            unsafe {
                renderer
                    .device
                    .queue_submit(*queue, &[submit], submit_fence.handle)
                    .unwrap();
            }

            // to destroy the command buffer when it's finished
            {
                let device = Arc::clone(&renderer.device);
                thread::spawn(move || unsafe {
                    device
                        .wait_for_fences(&[submit_fence.handle], true, u64::MAX)
                        .expect("Wait for fence failed.");
                    drop(command_buffer);
                    drop(submit_fence);
                });
            }
        } else {
            consolidate_vertex_buffers.sync_point = None;
        }
    }
}
