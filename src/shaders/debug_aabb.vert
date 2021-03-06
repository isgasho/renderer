#version 450

layout(push_constant) uniform PushConstants {
    vec4 center;
    vec4 half_extent;
};

layout(set = 0, binding = 0) uniform CameraMatrices {
    mat4 projection;
    mat4 view;
    vec4 pos;
};

void main() {
    vec4 min = center - half_extent;
    vec4 max = center + half_extent;

    vec3 points[8] = {
        // bottom half (min y)
        vec3(min.x, min.y, min.z),
        vec3(max.x, min.y, min.z),
        vec3(min.x, min.y, max.z),
        vec3(max.x, min.y, max.z),
        // top half (max y)
        vec3(min.x, max.y, min.z),
        vec3(max.x, max.y, min.z),
        vec3(min.x, max.y, max.z),
        vec3(max.x, max.y, max.z)
    };

    uint indices[36] = {
        // bottom - min y
        0, 1, 2,
        2, 1, 3,
        // top - max y
        6, 5, 4,
        7, 5, 6,
        // front - min z
        0, 4, 5,
        0, 5, 1,
        // back - max z
        7, 6, 2,
        3, 7, 2,
        // right - max x
        1, 5, 7,
        1, 7, 3,
        // left - min x
        6, 4, 0,
        6, 0, 2
    };

    vec3 position = points[indices[gl_VertexIndex]];

    gl_Position = projection * view * vec4(position, 1.0);
}
