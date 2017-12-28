extern crate ash;
extern crate cgmath;
extern crate forward_renderer;
extern crate futures;
extern crate futures_cpupool;
extern crate gltf;
extern crate petgraph;
extern crate specs;

use forward_renderer::*;
use ecs::*;
use render_dag::*;
use mesh;

use ash::vk;
use ash::version::*;
use cgmath::Rotation3;
use std::default::Default;
use std::fs::OpenOptions;
use std::io::Write;
use std::ptr;
use std::mem;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, Copy)]
struct Vertex {
    pos: [f32; 4],
    color: [f32; 4],
}

fn main() {
    let base = ExampleBase::new(1920, 1080);
    let render_dag = {
        let mut builder = RenderDAGBuilder::new();
        let acquire_image = builder.add_node("acquire_image", Node::AcquirePresentImage);
        let swapchain_attachment = builder.add_node("swapchain_attachment", Node::SwapchainAttachment(0));
        let depth_attachment = builder.add_node("depth_attachment", Node::DepthAttachment(1));
        let command_buffer = builder.with_command_buffer("main_command_buffer");
        let renderpass = command_buffer.with_renderpass(&mut builder, "renderpass");
        let subpass = renderpass.with_subpass(&mut builder, "main_subpass", 1);
        let framebuffer = builder.add_node("framebuffer", Node::Framebuffer);
        renderpass.start_before(&mut builder, &framebuffer);
        let vertex_shader = builder.add_node(
            "vertex_shader",
            Node::VertexShader(PathBuf::from(env!("OUT_DIR")).join("simple_color.vert.spv")),
        );
        let geom_shader = builder.add_node(
            "geometry_shader",
            Node::GeometryShader(PathBuf::from(env!("OUT_DIR")).join("simple_color.geom.spv")),
        );
        let fragment_shader = builder.add_node(
            "fragment_shader",
            Node::FragmentShader(PathBuf::from(env!("OUT_DIR")).join("simple_color.frag.spv")),
        );
        let pipeline_layout = builder.add_node("pipeline_layout", Node::PipelineLayout);
        let vertex_binding = builder.add_node(
            "vertex_binding",
            Node::VertexInputBinding(0, 3 * 4, vk::VertexInputRate::Vertex),
        );
        let uv_binding = builder.add_node(
            "uv_binding",
            Node::VertexInputBinding(1, 2 * 4, vk::VertexInputRate::Vertex),
        );
        let normal_binding = builder.add_node(
            "normal_binding",
            Node::VertexInputBinding(2, 3 * 4, vk::VertexInputRate::Vertex),
        );
        let tangent_binding = builder.add_node(
            "tangent_binding",
            Node::VertexInputBinding(3, 4 * 4, vk::VertexInputRate::Vertex),
        );
        let vertex_attribute = builder.add_node(
            "vertex_attribute",
            Node::VertexInputAttribute(0, 0, vk::Format::R32g32b32Sfloat, 0),
        );
        let uv_attribute = builder.add_node(
            "uv_attribute",
            Node::VertexInputAttribute(1, 1, vk::Format::R32g32Sfloat, 0),
        );
        let normal_attribute = builder.add_node(
            "normal_attribute",
            Node::VertexInputAttribute(2, 2, vk::Format::R32g32b32Sfloat, 0),
        );
        let tangent_attribute = builder.add_node(
            "tangent_attribute",
            Node::VertexInputAttribute(3, 3, vk::Format::R32g32b32a32Sfloat, 0),
        );
        let graphics_pipeline = subpass.add_node(&mut builder, "graphics_pipeline", Node::GraphicsPipeline);
        let draw_commands = subpass.add_node(
            &mut builder,
            "draw_commands",
            Node::DrawCommands(Arc::new(|device, world, render_dag, command_buffer| {
                use specs::Join;
                let world = world
                    .read()
                    .expect("Failed to lock the world read in DrawCommands");
                for (ix, mesh) in world.read::<SimpleColorMesh>().join().enumerate() {
                    unsafe {
                        device.cmd_bind_descriptor_sets(
                            command_buffer,
                            vk::PipelineBindPoint::Graphics,
                            render_dag
                                .pipeline_layouts
                                .get("pipeline_layout")
                                .unwrap()
                                .clone(),
                            0,
                            &[
                                render_dag
                                    .descriptor_sets
                                    .get("main_descriptor_layout")
                                    .unwrap()[ix]
                                    .clone(),
                            ],
                            &[],
                        );
                        let bindings = vec![mesh.0.vertex_buffer.vk(), mesh.0.tex_coords.vk(), mesh.0.normal_buffer.vk(), mesh.0.tangent_buffer.vk()];
                        let offsets = vec![0, 0, 0, 0];
                        device.cmd_bind_vertex_buffers(command_buffer, 0, &bindings, &offsets);
                        device.cmd_bind_index_buffer(
                            command_buffer,
                            mesh.0.index_buffer.vk(),
                            0,
                            mesh.0.index_type,
                        );
                        device.cmd_draw_indexed(command_buffer, mesh.0.index_count, 1, 0, 0, 0);
                    }
                }
            })),
        );
        let present_image = builder.add_node("present_image", Node::PresentImage);
        command_buffer.end_before(&mut builder, &present_image);
        renderpass.start_after(&mut builder, &acquire_image);
        renderpass.end_before(&mut builder, &present_image);
        renderpass.start_after(&mut builder, &swapchain_attachment);
        renderpass.start_after(&mut builder, &depth_attachment);
        builder.add_edge(&acquire_image, &present_image);
        builder.add_edge(&vertex_shader, &graphics_pipeline);
        // builder.add_edge(&geom_shader, &graphics_pipeline);
        builder.add_edge(&fragment_shader, &graphics_pipeline);
        builder.add_edge(&vertex_binding, &graphics_pipeline);
        builder.add_edge(&uv_binding, &graphics_pipeline);
        builder.add_edge(&normal_binding, &graphics_pipeline);
        builder.add_edge(&tangent_binding, &graphics_pipeline);
        builder.add_edge(&vertex_attribute, &graphics_pipeline);
        builder.add_edge(&uv_attribute, &graphics_pipeline);
        builder.add_edge(&normal_attribute, &graphics_pipeline);
        builder.add_edge(&tangent_attribute, &graphics_pipeline);
        builder.add_edge(&pipeline_layout, &graphics_pipeline);
        builder.add_edge(&graphics_pipeline, &draw_commands);
        subpass.end_after(&mut builder, &draw_commands);

        {
            let triangle_subpass = renderpass.with_subpass(&mut builder, "triangle_subpass", 0);
            // Triangle mesh
            let triangle_vertex_shader = builder.add_node(
                "triangle_vertex_shader",
                Node::VertexShader(PathBuf::from(env!("OUT_DIR")).join("triangle.vert.spv")),
            );
            let triangle_fragment_shader = builder.add_node(
                "triangle_fragment_shader",
                Node::FragmentShader(PathBuf::from(env!("OUT_DIR")).join("triangle.frag.spv")),
            );
            let triangle_pipeline_layout = builder.add_node("triangle_pipeline_layout", Node::PipelineLayout);
            let triangle_vertex_binding = builder.add_node(
                "triangle_vertex_binding",
                Node::VertexInputBinding(0, 4 * 5, vk::VertexInputRate::Vertex),
            );
            let triangle_vertex_attribute_pos = builder.add_node(
                "triangle_vertex_attribute_pos",
                Node::VertexInputAttribute(0, 0, vk::Format::R32g32Sfloat, 0),
            );
            let triangle_vertex_attribute_color = builder.add_node(
                "triangle_vertex_attribute_color",
                Node::VertexInputAttribute(0, 1, vk::Format::R32g32b32Sfloat, 2 * 4),
            );
            let triangle_graphics_pipeline = triangle_subpass.add_node(
                &mut builder,
                "triangle_graphics_pipeline",
                Node::GraphicsPipeline,
            );
            let triangle_draw_commands = triangle_subpass.add_node(
                &mut builder,
                "triangle_draw_commands",
                Node::DrawCommands(Arc::new(|device, world, _render_dag, command_buffer| {
                    use specs::Join;
                    let world = world
                        .read()
                        .expect("Failed to lock the world read in DrawCommands");
                    for mesh in world.read::<TriangleMesh>().join() {
                        unsafe {
                            device.cmd_bind_vertex_buffers(command_buffer, 0, &[mesh.0.vertex_buffer.vk()], &[0]);
                            device.cmd_bind_index_buffer(
                                command_buffer,
                                mesh.0.index_buffer.vk(),
                                0,
                                mesh.0.index_type,
                            );
                            device.cmd_draw_indexed(command_buffer, mesh.0.index_count, 1, 0, 0, 0);
                        }
                    }
                })),
            );

            builder.add_edge(&triangle_vertex_shader, &triangle_graphics_pipeline);
            builder.add_edge(&triangle_fragment_shader, &triangle_graphics_pipeline);
            builder.add_edge(&triangle_vertex_binding, &triangle_graphics_pipeline);
            builder.add_edge(&triangle_vertex_attribute_pos, &triangle_graphics_pipeline);
            builder.add_edge(
                &triangle_vertex_attribute_color,
                &triangle_graphics_pipeline,
            );
            builder.add_edge(&triangle_pipeline_layout, &triangle_graphics_pipeline);
            builder.add_edge(&triangle_graphics_pipeline, &triangle_draw_commands);
            triangle_subpass.end_after(&mut builder, &triangle_draw_commands);
            triangle_subpass.end_before(&mut builder, &subpass.begin);
        }

        let mvp_ubo = builder.add_node(
            "mvp_ubo",
            Node::DescriptorBinding(
                0,
                vk::DescriptorType::UniformBuffer,
                vk::SHADER_STAGE_VERTEX_BIT,
                1,
            ),
        );
        let color_texture = builder.add_node(
            "color_texture",
            Node::DescriptorBinding(
                1,
                vk::DescriptorType::CombinedImageSampler,
                vk::SHADER_STAGE_FRAGMENT_BIT,
                1,
            ),
        );
        let normal_texture = builder.add_node(
            "normal_texture",
            Node::DescriptorBinding(
                2,
                vk::DescriptorType::CombinedImageSampler,
                vk::SHADER_STAGE_FRAGMENT_BIT,
                1,
            ),
        );
        let main_descriptor_layout = builder.add_node("main_descriptor_layout", Node::DescriptorSet(16));

        builder.add_edge(&mvp_ubo, &main_descriptor_layout);
        builder.add_edge(&color_texture, &main_descriptor_layout);
        builder.add_edge(&normal_texture, &main_descriptor_layout);
        builder.add_edge(&main_descriptor_layout, &pipeline_layout);
        {
            let dot = petgraph::dot::Dot::with_config(&builder.graph, &[petgraph::dot::Config::EdgeNoLabel]);
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open("builder.dot")
                .unwrap();
            write!(f, "{:?}", dot).unwrap();
        }
        builder.build(&base)
    };
    {
        let dot = petgraph::dot::Dot::with_config(&render_dag.graph, &[petgraph::dot::Config::EdgeNoLabel]);
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("runtime.dot")
            .unwrap();
        write!(f, "{:?}", dot).unwrap();
    }
    unsafe {
        let mut world = World::new(&base.device);

        let gltf_rotation = cgmath::Quaternion::from_angle_x(cgmath::Deg(180.0));

        world
            .create_entity()
            .with::<Position>(Position(cgmath::Vector3::new(2.0, -1.5, 0.0)))
            .with::<Rotation>(Rotation(gltf_rotation))
            .with::<Scale>(Scale(70.0))
            .with::<Matrices>(Matrices::one())
            .with::<SimpleColorMesh>(SimpleColorMesh(
                mesh::Mesh::from_gltf(&base, "glTF-Sample-Models/2.0/BoomBox/glTF/BoomBox.gltf").unwrap(),
            ))
            .with::<TriangleMesh>(TriangleMesh(mesh::TriangleMesh::dummy(&base)))
            .build();

        world
            .create_entity()
            .with::<Position>(Position(cgmath::Vector3::new(2.0, 1.0, 0.0)))
            .with::<Rotation>(Rotation(gltf_rotation))
            .with::<Scale>(Scale(10.0))
            .with::<Matrices>(Matrices::one())
            .with::<SimpleColorMesh>(SimpleColorMesh(
                mesh::Mesh::from_gltf(
                    &base,
                    "glTF-Sample-Models/2.0/Avocado/glTF/Avocado.gltf",
                ).unwrap(),
            ))
            .build();

        world
            .create_entity()
            .with::<Position>(Position(cgmath::Vector3::new(-3.0, 0.0, 0.0)))
            .with::<Rotation>(Rotation(gltf_rotation))
            .with::<Scale>(Scale(2.0))
            .with::<Matrices>(Matrices::one())
            .with::<SimpleColorMesh>(SimpleColorMesh(
                mesh::Mesh::from_gltf(
                    &base,
                    "glTF-Sample-Models/2.0/SciFiHelmet/glTF/SciFiHelmet.gltf",
                ).unwrap(),
            ))
            .build();

        {
            use specs::Join;
            for (ix, mesh) in world.read::<SimpleColorMesh>().join().enumerate() {
                let mut textures = vec![];
                let create_info = vk::ImageViewCreateInfo {
                    s_type: vk::StructureType::ImageViewCreateInfo,
                    p_next: ptr::null(),
                    flags: Default::default(),
                    image: mesh.0.base_color_image.image,
                    view_type: vk::ImageViewType::Type2d,
                    format: vk::Format::R8g8b8a8Unorm,
                    components: vk::ComponentMapping {
                        r: vk::ComponentSwizzle::Identity,
                        g: vk::ComponentSwizzle::Identity,
                        b: vk::ComponentSwizzle::Identity,
                        a: vk::ComponentSwizzle::Identity,
                    },
                    subresource_range: vk::ImageSubresourceRange {
                        aspect_mask: vk::IMAGE_ASPECT_COLOR_BIT,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                };
                textures.push((
                    mesh.0.texture_sampler,
                    base.device.create_image_view(&create_info, None).unwrap(),
                ));
                {
                    let create_info = vk::ImageViewCreateInfo {
                        s_type: vk::StructureType::ImageViewCreateInfo,
                        p_next: ptr::null(),
                        flags: Default::default(),
                        image: mesh.0.normal_texture.image,
                        view_type: vk::ImageViewType::Type2d,
                        format: vk::Format::R8g8b8a8Unorm,
                        components: vk::ComponentMapping {
                            r: vk::ComponentSwizzle::Identity,
                            g: vk::ComponentSwizzle::Identity,
                            b: vk::ComponentSwizzle::Identity,
                            a: vk::ComponentSwizzle::Identity,
                        },
                        subresource_range: vk::ImageSubresourceRange {
                            aspect_mask: vk::IMAGE_ASPECT_COLOR_BIT,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        },
                    };
                    textures.push((
                        mesh.0.texture_sampler,
                        base.device.create_image_view(&create_info, None).unwrap(),
                    ));
                }
                let descriptor_set = render_dag
                    .descriptor_sets
                    .get("main_descriptor_layout")
                    .unwrap()[ix];
                let mut image_infos = vec![];
                let write_sets = textures
                    .iter()
                    .enumerate()
                    .map(|(ix, &(sampler, texture))| {
                        image_infos.push(vk::DescriptorImageInfo {
                            sampler: sampler,
                            image_view: texture,
                            image_layout: vk::ImageLayout::ShaderReadOnlyOptimal,
                        });
                        let image_info = image_infos.last().unwrap();
                        vk::WriteDescriptorSet {
                            s_type: vk::StructureType::WriteDescriptorSet,
                            p_next: ptr::null(),
                            dst_set: descriptor_set.clone(),
                            dst_binding: 1 + ix as u32,
                            dst_array_element: 0,
                            descriptor_count: 1,
                            descriptor_type: vk::DescriptorType::CombinedImageSampler,
                            p_image_info: image_info,
                            p_buffer_info: ptr::null(),
                            p_texel_buffer_view: ptr::null(),
                        }
                    })
                    .collect::<Vec<_>>();
                println!("write_sets {:?}", write_sets);
                base.device.update_descriptor_sets(&write_sets, &[]);
            }
        }

        let world = Arc::new(RwLock::new(world));

        let projection = cgmath::perspective(
            cgmath::Deg(60.0),
            base.surface_resolution.width as f32 / base.surface_resolution.height as f32,
            0.1,
            100.0,
        );
        let view = cgmath::Matrix4::look_at(
            cgmath::Point3::new(0.0, 0.0, -5.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            cgmath::vec3(0.0, 1.0, 0.0),
        );
        let mut dispatcher = specs::DispatcherBuilder::new()
            .add(SteadyRotation, "steady_rotation", &[])
            .add(
                MVPCalculation { projection, view },
                "mvp",
                &["steady_rotation"],
            )
            .build();
        base.render_loop(&mut || {
            {
                let mut world = world
                    .write()
                    .expect("failed to lock write world in render loop");
                dispatcher.dispatch(&mut world.res);
            }
            let mut buffers = vec![];
            {
                use specs::Join;
                let world = world
                    .read()
                    .expect("failed to lock read world in render loop");
                for (ix, mvp) in world.read::<Matrices>().join().enumerate() {
                    let mvps = [mvp.clone()];
                    let ubo_buffer = buffer::Buffer::upload_from::<Matrices, _>(
                        &base,
                        vk::BUFFER_USAGE_UNIFORM_BUFFER_BIT,
                        &mvps.iter().cloned(),
                    );
                    let ubo_mvp = render_dag
                        .descriptor_sets
                        .get("main_descriptor_layout")
                        .unwrap()[ix];
                    base.device.update_descriptor_sets(
                        &[
                            vk::WriteDescriptorSet {
                                s_type: vk::StructureType::WriteDescriptorSet,
                                p_next: ptr::null(),
                                dst_set: ubo_mvp.clone(),
                                dst_binding: 0,
                                dst_array_element: 0,
                                descriptor_count: 1,
                                descriptor_type: vk::DescriptorType::UniformBuffer,
                                p_image_info: ptr::null(),
                                p_buffer_info: &vk::DescriptorBufferInfo {
                                    buffer: ubo_buffer.vk(),
                                    offset: 0,
                                    range: (mem::size_of::<Matrices>()) as u64,
                                },
                                p_texel_buffer_view: ptr::null(),
                            },
                        ],
                        &[],
                    );
                    buffers.push(ubo_buffer);
                }
            }

            /*
            record_submit_commandbuffer(
                base.device.vk(),
                base.draw_command_buffer,
                base.present_queue,
                &[vk::PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT],
                &[base.present_complete_semaphore],
                &[base.rendering_complete_semaphore],
                |_device, draw_command_buffer| {
                    render_dag.run(&base, &world, draw_command_buffer);
                },
            );
            */

            render_dag.run(&base, &world).expect("RenderDAG failed");
            println!("render_dag executed");

            /*
            command_buffer::one_time_submit_and_wait(
                &base,
                |cmd_buffer| {
                    render_dag.run(&base, &world, cmd_buffer);
                }
            );
            */

            for buffer in buffers.into_iter() {
                buffer.free(base.device.vk())
            }
        });

        base.device.device_wait_idle().unwrap();
    }
}
