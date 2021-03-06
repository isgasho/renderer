use ash::{version::DeviceV1_0, vk};
use gltf;
use image;
use meshopt;
use std::{mem::size_of, path::Path, u64};

use super::{
    alloc,
    device::{Buffer, Image},
    GraphicsCommandPool, RenderFrame,
};

pub struct LoadedMesh {
    pub vertex_buffer: Buffer,
    pub normal_buffer: Buffer,
    pub uv_buffer: Buffer,
    pub index_buffers: Vec<(Buffer, u64)>,
    pub vertex_len: u64,
    pub aabb: ncollide3d::bounding_volume::AABB<f32>,
    pub base_color: Image,
}

#[derive(Clone, Default)]
struct Pos(pub [f32; 3]);

impl meshopt::DecodePosition for Pos {
    fn decode_position(&self) -> [f32; 3] {
        self.0
    }
}

pub fn load(
    renderer: &RenderFrame,
    graphics_command_pool: &GraphicsCommandPool,
    path: &str,
) -> LoadedMesh {
    let (loaded, buffers, _images) = gltf::import(path).expect("Failed loading mesh");
    let mesh = loaded
        .meshes()
        .next()
        .expect("failed to get first mesh from gltf");
    let primitive = mesh
        .primitives()
        .next()
        .expect("failed to get first primitive from gltf");
    let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
    let positions = reader
        .read_positions()
        .expect("failed to load positions")
        .map(Pos)
        .collect::<Vec<_>>();
    let uvs = reader
        .read_tex_coords(0)
        .expect("failed to load uvs")
        .into_f32()
        .collect::<Vec<_>>();
    let bounding_box = primitive.bounding_box();
    let aabb = ncollide3d::bounding_volume::AABB::new(
        na::Point3::from(bounding_box.min),
        na::Point3::from(bounding_box.max),
    );
    let normals = reader
        .read_normals()
        .expect("failed to load normals")
        .collect::<Vec<_>>();
    let indices = reader
        .read_indices()
        .expect("failed to load indices")
        .into_u32()
        .collect::<Vec<_>>();
    let base_color_source = primitive
        .material()
        .pbr_metallic_roughness()
        .base_color_texture()
        .expect("failed to load base color")
        .texture()
        .source()
        .source();
    let base_color_image = match base_color_source {
        gltf::image::Source::Uri { uri, .. } => {
            image::open(Path::new(path).parent().unwrap().join(uri))
                .expect("failed to open base color texture")
                .to_rgba()
        }
        gltf::image::Source::View { .. } => {
            unimplemented!("Reading embedded textures in gltf not supported")
        }
    };
    let base_color_vkimage = renderer.device.new_image(
        vk::Format::R8G8B8A8_UNORM,
        vk::Extent3D {
            height: base_color_image.height(),
            width: base_color_image.width(),
            depth: 1,
        },
        vk::SampleCountFlags::TYPE_1,
        vk::ImageTiling::LINEAR, // todo use optimal?
        vk::ImageLayout::PREINITIALIZED,
        vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
    );
    renderer
        .device
        .set_object_name(base_color_vkimage.handle, "Gltf mesh Base color image");
    let base_color_upload_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::TRANSFER_SRC,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
        vk::DeviceSize::from(base_color_image.width())
            * vk::DeviceSize::from(base_color_image.height())
            * 4,
    );
    renderer.device.set_object_name(
        base_color_upload_buffer.handle,
        "Gltf mesh Base color image Upload Buffer",
    );
    {
        let mut mapped = base_color_upload_buffer
            .map::<image::Rgba<u8>>()
            .expect("Failed to map base color upload buffer");
        for (ix, pixel) in base_color_image.pixels().enumerate() {
            mapped[ix] = *pixel;
        }
    }
    let (a, meshoptpositions, b) = unsafe { positions.as_slice().align_to::<u8>() };
    assert_eq!(a.len(), 0);
    assert_eq!(b.len(), 0);
    let vertex_adapter =
        meshopt::VertexDataAdapter::new(&meshoptpositions, size_of::<f32>() * 3, 0)
            .expect("vertex data adapter failed");
    let mut index_lods = if indices.len() > 1000
    /* disable temporarily */
    {
        let mut lods = Vec::with_capacity(6);
        for x in 1..6 {
            let factor = (2 as usize).pow(x);
            lods.push(meshopt::simplify::simplify_sloppy(
                &indices,
                &vertex_adapter,
                indices.len() / factor,
            ));
        }
        lods
    } else {
        vec![indices]
    };
    for mut indices in index_lods.iter_mut() {
        // This is a bug in the library
        #[allow(clippy::unnecessary_mut_passed)]
        meshopt::optimize_vertex_cache_in_place(&mut indices, positions.len());
        // meshopt::optimize_overdraw_in_place(&mut indices, &meshopt::VertexDataAdapter::new(&positions, size_of::<f32>(), 0), 1.05);
    }
    /*
    // quoting meshopt:
    When a sequence of LOD meshes is generated that all use the original vertex buffer, care must be taken to order vertices optimally to not penalize mobile GPU architectures that are only capable of transforming a sequential vertex buffer range. It's recommended in this case to first optimize each LOD for vertex cache, then assemble all LODs in one large index buffer starting from the coarsest LOD (the one with fewest triangles), and call meshopt_optimizeVertexFetch on the final large index buffer. This will make sure that coarser LODs require a smaller vertex range and are efficient wrt vertex fetch and transform.
    let remap = meshopt::optimize_vertex_fetch_remap(&indices, positions.len());
    let indices_new = meshopt::remap_index_buffer(Some(&indices), positions.len(), &remap);
    let positions_new = meshopt::remap_vertex_buffer(&positions, positions.len(), &remap);
    let uvs_new = meshopt::remap_vertex_buffer(&uvs, positions.len(), &remap);
    let normals_new = meshopt::remap_vertex_buffer(&normals, positions.len(), &remap);

    // shadow with optimized buffers
    let indices = indices_new;
    let normals = normals_new;
    let uvs = uvs_new;
    let positions = positions_new;
    */

    let vertex_len = positions.len() as u64;
    let vertex_size = size_of::<f32>() as u64 * 3 * vertex_len;
    let normals_size = size_of::<f32>() as u64 * 3 * vertex_len;
    let uvs_size = size_of::<f32>() as u64 * 2 * vertex_len;
    let vertex_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::VERTEX_BUFFER
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC
            | vk::BufferUsageFlags::STORAGE_BUFFER,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
        vertex_size,
    );
    renderer
        .device
        .set_object_name(vertex_buffer.handle, "Gltf mesh Vertex buffer");
    let vertex_upload_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::TRANSFER_SRC,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
        vertex_size,
    );
    renderer.device.set_object_name(
        vertex_upload_buffer.handle,
        "Gltf mesh Vertex upload buffer",
    );
    {
        let mut mapped = vertex_upload_buffer
            .map::<[f32; 3]>()
            .expect("Failed to map vertex upload buffer");
        for (ix, data) in positions.iter().enumerate() {
            mapped[ix] = data.0;
        }
    }
    let normal_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::VERTEX_BUFFER
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
        normals_size,
    );
    renderer
        .device
        .set_object_name(normal_buffer.handle, "Gltf mesh Normal buffer");
    let normal_upload_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::TRANSFER_SRC,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
        normals_size,
    );
    renderer.device.set_object_name(
        normal_upload_buffer.handle,
        "Gltf mesh Normal upload buffer",
    );
    {
        let mut mapped = normal_upload_buffer
            .map::<[f32; 3]>()
            .expect("Failed to map normal upload buffer");
        for (ix, data) in normals.iter().enumerate() {
            mapped[ix] = *data;
        }
    }
    let uv_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::VERTEX_BUFFER
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
        uvs_size,
    );
    renderer
        .device
        .set_object_name(uv_buffer.handle, "Gltf mesh UV buffer");
    let uv_upload_buffer = renderer.device.new_buffer(
        vk::BufferUsageFlags::TRANSFER_SRC,
        alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
        uvs_size,
    );
    renderer
        .device
        .set_object_name(uv_upload_buffer.handle, "Gltf mesh UV upload buffer");
    {
        let mut mapped = uv_upload_buffer
            .map::<[f32; 2]>()
            .expect("Failed to map UV upload buffer");
        for (ix, data) in uvs.iter().enumerate() {
            mapped[ix] = *data;
        }
    }
    let index_buffers = index_lods
        .iter()
        .enumerate()
        .map(|(ix, indices)| {
            let index_len = indices.len() as u64;
            let index_size = size_of::<u32>() as u64 * index_len;
            let index_buffer = renderer.device.new_buffer(
                vk::BufferUsageFlags::INDEX_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
                alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_GPU_ONLY,
                index_size,
            );
            renderer.device.set_object_name(
                index_buffer.handle,
                &format!("Gltf mesh index buffer LOD {}", ix),
            );
            let index_upload_buffer = renderer.device.new_buffer(
                vk::BufferUsageFlags::TRANSFER_SRC,
                alloc::VmaMemoryUsage::VMA_MEMORY_USAGE_CPU_TO_GPU,
                index_size,
            );
            renderer.device.set_object_name(
                index_upload_buffer.handle,
                &format!("Gltf mesh index upload buffer LOD {}", ix),
            );
            {
                let mut mapped = index_upload_buffer
                    .map::<u32>()
                    .expect("failed to map index upload buffer");
                mapped[0..index_len as usize].copy_from_slice(&indices);
            }

            (index_buffer, index_upload_buffer, index_len)
        })
        .collect::<Vec<_>>();
    let upload = graphics_command_pool
        .0
        .record_one_time("upload gltf mesh cb", {
            let vertex_buffer = &vertex_buffer;
            let vertex_upload_buffer = &vertex_upload_buffer;
            let normal_buffer = &normal_buffer;
            let normal_upload_buffer = &normal_upload_buffer;
            let uv_buffer = &uv_buffer;
            let uv_upload_buffer = &uv_upload_buffer;
            let index_buffers = &index_buffers;
            let base_color_upload_buffer = &base_color_upload_buffer;
            let base_color_vkimage = &base_color_vkimage;
            let device = &renderer.device;
            move |command_buffer| unsafe {
                device.device.cmd_copy_buffer(
                    command_buffer,
                    vertex_upload_buffer.handle,
                    vertex_buffer.handle,
                    &[vk::BufferCopy {
                        src_offset: 0,
                        dst_offset: 0,
                        size: vertex_size,
                    }],
                );
                device.device.cmd_copy_buffer(
                    command_buffer,
                    normal_upload_buffer.handle,
                    normal_buffer.handle,
                    &[vk::BufferCopy {
                        src_offset: 0,
                        dst_offset: 0,
                        size: normals_size,
                    }],
                );
                device.device.cmd_copy_buffer(
                    command_buffer,
                    uv_upload_buffer.handle,
                    uv_buffer.handle,
                    &[vk::BufferCopy {
                        src_offset: 0,
                        dst_offset: 0,
                        size: uvs_size,
                    }],
                );
                for (index_buffer, index_upload_buffer, index_len) in index_buffers.iter() {
                    let index_size = size_of::<u32>() as u64 * index_len;
                    device.device.cmd_copy_buffer(
                        command_buffer,
                        index_upload_buffer.handle,
                        index_buffer.handle,
                        &[vk::BufferCopy {
                            src_offset: 0,
                            dst_offset: 0,
                            size: index_size,
                        }],
                    );
                }
                device.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[vk::ImageMemoryBarrier::builder()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .old_layout(vk::ImageLayout::PREINITIALIZED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(base_color_vkimage.handle)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .build()],
                );
                device.device.cmd_copy_buffer_to_image(
                    command_buffer,
                    base_color_upload_buffer.handle,
                    base_color_vkimage.handle,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::BufferImageCopy::builder()
                        .image_extent(vk::Extent3D {
                            height: base_color_image.height(),
                            width: base_color_image.width(),
                            depth: 1,
                        })
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .build()],
                );
                device.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[vk::ImageMemoryBarrier::builder()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(base_color_vkimage.handle)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .build()],
                );
            }
        });
    let mut graphics_queue = renderer.device.graphics_queue.lock();
    let upload_fence = upload.submit_once(&mut *graphics_queue, "upload gltf mesh commands");
    unsafe {
        renderer
            .device
            .wait_for_fences(&[upload_fence.handle], true, u64::MAX)
            .expect("Wait for fence failed.");
    }
    let index_buffers = index_buffers
        .into_iter()
        .map(|(buffer, _, len)| (buffer, len))
        .collect();

    LoadedMesh {
        vertex_buffer,
        normal_buffer,
        uv_buffer,
        index_buffers,
        vertex_len,
        aabb,
        base_color: base_color_vkimage,
    }
}
