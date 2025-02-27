use std::ops::Range;

use bevy::{
    core_pipeline::core_3d::graph::{Core3d, Node3d},
    ecs::{
        query::ROQueryItem,
        system::{
            lifetimeless::{Read, SRes},
            SystemParamItem,
        },
    },
    pbr::{
        setup_morph_and_skinning_defs, DrawMesh, MaterialPipeline, MaterialPipelineKey,
        MeshLayouts, MeshPipeline, MeshPipelineKey, RenderMaterialInstances, RenderMaterials,
        RenderMeshInstances, SetMaterialBindGroup, SetMeshBindGroup, MAX_CASCADES_PER_LIGHT,
        MAX_DIRECTIONAL_LIGHTS,
    },
    prelude::*,
    render::{
        batching::batch_and_prepare_render_phase,
        camera::CameraProjection,
        mesh::MeshVertexBufferLayout,
        render_asset::RenderAssets,
        render_graph::{Node, RenderGraph, RenderLabel},
        render_phase::{
            AddRenderCommand, CachedRenderPipelinePhaseItem, DrawFunctionId, DrawFunctions,
            PhaseItem, RenderCommand, RenderCommandResult, RenderPhase, SetItemPipeline,
        },
        render_resource::{
            AddressMode, AsBindGroup, BindGroup, BindGroupEntries, BindGroupLayout,
            BindGroupLayoutEntry, BindingType, BufferBindingType, BufferSize,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FilterMode,
            FragmentState, FrontFace, LoadOp, MultisampleState, Operations, PipelineCache,
            PolygonMode, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor,
            RenderPipelineDescriptor, Sampler, SamplerDescriptor, ShaderDefVal, ShaderStages,
            ShaderType, SpecializedMeshPipeline, SpecializedMeshPipelineError,
            SpecializedMeshPipelines, StoreOp, TextureDescriptor, TextureDimension, TextureFormat,
            TextureUsages, TextureView, VertexState,
        },
        renderer::RenderDevice,
        texture::TextureCache,
        view::{
            prepare_view_uniforms, ExtractedView, ExtractedWindows, ViewUniform, ViewUniformOffset,
            ViewUniforms, VisibleEntities,
        },
        Render, RenderApp, RenderSet,
    },
    utils::{nonmax::NonMaxU32, FloatOrd},
};

use crate::{GlobalInfiniteGridSettings, GridFrustumIntersect};

use super::{
    ExtractedInfiniteGrid, GridShadowUniformOffset, GridShadowUniforms, InfiniteGridPipeline,
};

static SHADOW_RENDER: &str = include_str!("shadow_render.wgsl");

const SHADOW_SHADER_HANDLE: Handle<Shader> = Handle::weak_from_u128(10461510954165139918);

pub struct GridShadow {
    pub entity: Entity,
    pub pipeline: CachedRenderPipelineId,
    pub draw_function: DrawFunctionId,
    pub batch_range: Range<u32>,
    pub dynamic_offset: Option<NonMaxU32>,
}

impl PhaseItem for GridShadow {
    type SortKey = FloatOrd;

    #[inline]
    fn entity(&self) -> Entity {
        self.entity
    }

    #[inline]
    fn sort_key(&self) -> Self::SortKey {
        unimplemented!("grid shadows don't need sorting")
    }

    #[inline]
    fn draw_function(&self) -> DrawFunctionId {
        self.draw_function
    }

    fn batch_range(&self) -> &Range<u32> {
        &self.batch_range
    }

    fn batch_range_mut(&mut self) -> &mut Range<u32> {
        &mut self.batch_range
    }

    fn dynamic_offset(&self) -> Option<NonMaxU32> {
        self.dynamic_offset
    }

    fn dynamic_offset_mut(&mut self) -> &mut Option<NonMaxU32> {
        &mut self.dynamic_offset
    }
}

impl CachedRenderPipelinePhaseItem for GridShadow {
    #[inline]
    fn cached_pipeline(&self) -> CachedRenderPipelineId {
        self.pipeline
    }
}

#[derive(Resource)]
pub struct GridShadowPipeline {
    pub view_layout: BindGroupLayout,
    pub material_layout: BindGroupLayout,
    pub material_pipeline: MaterialPipeline<StandardMaterial>,
    pub mesh_layouts: MeshLayouts,
    pub sampler: Sampler,
}

impl FromWorld for GridShadowPipeline {
    fn from_world(world: &mut World) -> Self {
        let world = world.cell();
        let render_device = world.get_resource::<RenderDevice>().unwrap();

        let view_layout = render_device.create_bind_group_layout(
            "grid_shadow_view_layout",
            &[
                // View
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: BufferSize::new(ViewUniform::min_size().into()),
                    },
                    count: None,
                },
            ],
        );

        let mesh_pipeline = world.get_resource::<MeshPipeline>().unwrap();
        let material_pipeline = world
            .get_resource::<MaterialPipeline<StandardMaterial>>()
            .unwrap()
            .clone();

        GridShadowPipeline {
            view_layout,
            mesh_layouts: mesh_pipeline.mesh_layouts.clone(),
            sampler: render_device.create_sampler(&SamplerDescriptor {
                address_mode_u: AddressMode::ClampToEdge,
                address_mode_v: AddressMode::ClampToEdge,
                address_mode_w: AddressMode::ClampToEdge,
                mag_filter: FilterMode::Linear,
                min_filter: FilterMode::Linear,
                mipmap_filter: FilterMode::Nearest,
                compare: None,
                ..Default::default()
            }),
            material_layout: StandardMaterial::bind_group_layout(&render_device),
            material_pipeline,
        }
    }
}

impl SpecializedMeshPipeline for GridShadowPipeline {
    type Key = MaterialPipelineKey<StandardMaterial>;

    fn specialize(
        &self,
        key: Self::Key,
        layout: &MeshVertexBufferLayout,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        let mut vertex_attributes = vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0)];

        let mut bind_group_layouts = vec![self.view_layout.clone()];

        bind_group_layouts.insert(1, self.material_layout.clone());

        let mut shader_defs = vec![
            ShaderDefVal::UInt(
                "MAX_DIRECTIONAL_LIGHTS".to_string(),
                MAX_DIRECTIONAL_LIGHTS as u32,
            ),
            ShaderDefVal::UInt(
                "MAX_CASCADES_PER_LIGHT".to_string(),
                MAX_CASCADES_PER_LIGHT as u32,
            ),
        ];

        bind_group_layouts.insert(
            2,
            setup_morph_and_skinning_defs(
                &self.mesh_layouts,
                layout,
                4,
                &key.mesh_key,
                &mut shader_defs,
                &mut vertex_attributes,
            ),
        );

        let vertex_buffer_layout = layout.get_layout(&vertex_attributes)?;

        let mut descriptor = RenderPipelineDescriptor {
            vertex: VertexState {
                shader: SHADOW_SHADER_HANDLE,
                entry_point: "vertex".into(),
                shader_defs: shader_defs.clone(),
                buffers: vec![vertex_buffer_layout],
            },
            fragment: Some(FragmentState {
                shader: SHADOW_SHADER_HANDLE,
                shader_defs,
                entry_point: "fragment".into(),
                targets: vec![Some(ColorTargetState {
                    format: TextureFormat::R8Unorm,
                    blend: None,
                    write_mask: ColorWrites::RED,
                })],
            }),
            layout: bind_group_layouts,
            push_constant_ranges: Vec::new(),
            primitive: PrimitiveState {
                topology: key.mesh_key.primitive_topology(),
                strip_index_format: None,
                front_face: FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            label: Some("grid_shadow_pipeline".into()),
        };

        StandardMaterial::specialize(&self.material_pipeline, &mut descriptor, layout, key)?;

        Ok(descriptor)
    }
}

#[derive(Resource, Default)]
struct GridShadowMeta {
    view_bind_group: Option<BindGroup>,
}

type DrawGridShadowMesh = (
    SetItemPipeline,
    SetGridShadowViewBindGroup<0>,
    SetMaterialBindGroup<StandardMaterial, 1>,
    SetMeshBindGroup<2>,
    DrawMesh,
);

struct SetGridShadowViewBindGroup<const I: usize>;

impl<const I: usize, P: PhaseItem> RenderCommand<P> for SetGridShadowViewBindGroup<I> {
    type Param = SRes<GridShadowMeta>;
    type ViewQuery = Read<ViewUniformOffset>;
    type ItemQuery = ();

    #[inline]
    fn render<'w>(
        _item: &P,
        view_uniform_offset: ROQueryItem<'w, Self::ViewQuery>,
        _entity: ROQueryItem<'w, Option<Self::ItemQuery>>,
        meta: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut bevy::render::render_phase::TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        pass.set_bind_group(
            I,
            meta.into_inner().view_bind_group.as_ref().unwrap(),
            &[view_uniform_offset.offset],
        );

        RenderCommandResult::Success
    }
}

#[derive(Component)]
struct GridShadowView {
    texture_view: TextureView,
}

fn prepare_grid_shadow_views(
    mut commands: Commands,
    grids: Query<(Entity, &ExtractedInfiniteGrid, &GridFrustumIntersect)>,
    render_device: Res<RenderDevice>,
    mut texture_cache: ResMut<TextureCache>,
    windows: Res<ExtractedWindows>,
    settings: Res<RenderSettings>,
) {
    let primary_window = if let Some(w) = windows.primary.as_ref().and_then(|id| windows.get(id)) {
        w
    } else {
        return;
    };
    let width = primary_window.physical_width;
    let height = primary_window.physical_height;
    let comp = width < height;
    let [min, max] = if comp {
        [width, height]
    } else {
        [height, width]
    };
    let ratio = min as f32 / max as f32;
    let tmax = settings.max_texture_size;
    let tmin = (tmax as f32 * ratio) as u32;
    let [width, height] = if comp { [tmin, tmax] } else { [tmax, tmin] };
    for (entity, grid, frustum_intersect) in grids.iter() {
        let texture = texture_cache.get(
            &render_device,
            TextureDescriptor {
                label: Some("grid_shadow_texture"),
                size: Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TextureFormat::R8Unorm,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );

        let projection = OrthographicProjection {
            area: Rect::new(
                // left, bottom, right, top
                frustum_intersect.width / -2.,
                frustum_intersect.height / -2.,
                frustum_intersect.width / 2.,
                frustum_intersect.height / 2.,
            ),
            ..Default::default()
        };

        commands.entity(entity).insert((
            ExtractedView {
                projection: projection.get_projection_matrix(),
                transform: Transform::from_translation(
                    frustum_intersect.center + grid.transform.up() * 500.,
                )
                .looking_at(frustum_intersect.center, frustum_intersect.up_dir)
                .into(),
                view_projection: None,
                hdr: false,
                viewport: UVec4::new(0, 0, width, height),
                color_grading: Default::default(),
            },
            GridShadowView {
                texture_view: texture.default_view.clone(),
            },
        ));
    }
}

fn prepare_grid_shadow_view_bind_group(
    render_device: Res<RenderDevice>,
    shadow_pipeline: Res<GridShadowPipeline>,
    mut meta: ResMut<GridShadowMeta>,
    view_uniforms: Res<ViewUniforms>,
) {
    if let Some(view_binding) = view_uniforms.uniforms.binding() {
        meta.view_bind_group = Some(render_device.create_bind_group(
            "grid_shadow_view_bind_group",
            &shadow_pipeline.view_layout,
            &BindGroupEntries::single(view_binding),
        ));
    }
}

#[derive(Component)]
pub struct GridShadowBindGroup {
    bind_group: BindGroup,
}

fn prepare_grid_shadow_bind_groups(
    mut commands: Commands,
    grids: Query<(Entity, &GridShadowView)>,
    uniforms: Res<GridShadowUniforms>,
    infinite_grid_pipeline: Res<InfiniteGridPipeline>,
    grid_shadow_pipeline: Res<GridShadowPipeline>,
    render_device: Res<RenderDevice>,
) {
    if let Some(uniform_binding) = uniforms.uniforms.binding() {
        for (entity, shadow_view) in grids.iter() {
            let bind_group = render_device.create_bind_group(
                "grid-shadow-bind-group",
                &infinite_grid_pipeline.grid_shadows_layout,
                &BindGroupEntries::sequential((
                    uniform_binding.clone(),
                    &shadow_view.texture_view,
                    &grid_shadow_pipeline.sampler,
                )),
            );
            commands
                .entity(entity)
                .insert(GridShadowBindGroup { bind_group });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn queue_grid_shadows(
    mut grids: Query<(&mut RenderPhase<GridShadow>, &VisibleEntities)>,
    render_meshes: Res<RenderAssets<Mesh>>,
    render_mesh_instances: Res<RenderMeshInstances>,
    render_materials: Res<RenderMaterials<StandardMaterial>>,
    render_material_instances: Res<RenderMaterialInstances<StandardMaterial>>,
    mut pipelines: ResMut<SpecializedMeshPipelines<GridShadowPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    shadow_pipeline: Res<GridShadowPipeline>,
    shadow_draw_functions: Res<DrawFunctions<GridShadow>>,
) {
    let draw_shadow_mesh = shadow_draw_functions
        .read()
        .get_id::<DrawGridShadowMesh>()
        .unwrap();
    for (mut phase, entities) in grids.iter_mut() {
        for &entity in &entities.entities {
            if let (Some(mesh_instance), Some(material_asset_id)) = (
                render_mesh_instances.get(&entity),
                render_material_instances.get(&entity),
            ) {
                if !mesh_instance.shadow_caster {
                    continue;
                }

                if let (Some(mesh), Some(material)) = (
                    render_meshes.get(mesh_instance.mesh_asset_id),
                    render_materials.get(material_asset_id),
                ) {
                    let key = MaterialPipelineKey {
                        mesh_key: MeshPipelineKey::from_primitive_topology(mesh.primitive_topology),
                        bind_group_data: material.key.clone(),
                    };
                    let pipeline_id =
                        pipelines.specialize(&pipeline_cache, &shadow_pipeline, key, &mesh.layout);

                    let pipeline_id = match pipeline_id {
                        Ok(id) => id,
                        Err(err) => {
                            error!("{}", err);
                            continue;
                        }
                    };

                    phase.add(GridShadow {
                        draw_function: draw_shadow_mesh,
                        pipeline: pipeline_id,
                        entity,
                        batch_range: 0..1,
                        dynamic_offset: None,
                    });
                }
            }
        }
    }
}

pub struct SetGridShadowBindGroup<const I: usize>;

impl<const I: usize, P: PhaseItem> RenderCommand<P> for SetGridShadowBindGroup<I> {
    type Param = ();
    type ViewQuery = ();
    type ItemQuery = Option<(Read<GridShadowBindGroup>, Read<GridShadowUniformOffset>)>;

    #[inline]
    fn render<'w>(
        _item: &P,
        _view: ROQueryItem<'w, Self::ViewQuery>,
        bg_offset: ROQueryItem<'w, Option<Self::ItemQuery>>,
        _query: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut bevy::render::render_phase::TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        if let Some(Some((bg, offset))) = bg_offset {
            pass.set_bind_group(I, &bg.bind_group, &[offset.offset]);
        }
        RenderCommandResult::Success
    }
}

#[allow(clippy::type_complexity)]
struct GridShadowPassNode {
    grids: Vec<Entity>,
    grid_entity_query: QueryState<Entity, With<GridShadowView>>,
    grid_element_query: QueryState<(
        Read<GridShadowView>,
        Read<RenderPhase<GridShadow>>,
        Read<ViewUniformOffset>,
    )>,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct GridShadowPassLabel;

impl GridShadowPassNode {
    fn new(world: &mut World) -> Self {
        Self {
            grids: Vec::new(),
            grid_entity_query: world.query_filtered(),
            grid_element_query: world.query(),
        }
    }
}

impl Node for GridShadowPassNode {
    fn update(&mut self, world: &mut World) {
        self.grids.clear();
        self.grids.extend(self.grid_entity_query.iter(world));
        self.grid_element_query.update_archetypes(world);
    }

    fn run(
        &self,
        _graph: &mut bevy::render::render_graph::RenderGraphContext,
        render_context: &mut bevy::render::renderer::RenderContext,
        world: &World,
    ) -> Result<(), bevy::render::render_graph::NodeRunError> {
        for &entity in &self.grids {
            let (shadow_view, render_phase, _) =
                self.grid_element_query.get_manual(world, entity).unwrap();
            let pass_descriptor = RenderPassDescriptor {
                label: Some("grid_shadow_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &shadow_view.texture_view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::BLACK.into()),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            };

            let mut tracked_render_pass = render_context.begin_tracked_render_pass(pass_descriptor);
            render_phase.render(&mut tracked_render_pass, world, entity);
        }

        Ok(())
    }
}

#[derive(Resource, Clone)]
pub struct RenderSettings {
    pub max_texture_size: u32,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            max_texture_size: 16384,
        }
    }
}

pub fn register_shadow(app: &mut App) {
    app.world
        .resource_mut::<Assets<Shader>>()
        .get_or_insert_with(SHADOW_SHADER_HANDLE, || {
            Shader::from_wgsl(SHADOW_RENDER, file!())
        });

    let render_settings = app
        .world
        .resource::<GlobalInfiniteGridSettings>()
        .render_settings
        .clone();

    let render_app = app.get_sub_app_mut(RenderApp).unwrap();
    render_app
        .init_resource::<GridShadowMeta>()
        .init_resource::<GridShadowPipeline>()
        .init_resource::<DrawFunctions<GridShadow>>()
        .init_resource::<SpecializedMeshPipelines<GridShadowPipeline>>()
        .insert_resource(render_settings)
        .add_render_command::<GridShadow, DrawGridShadowMesh>()
        .add_systems(
            Render,
            (prepare_grid_shadow_views, apply_deferred)
                .chain()
                .before(prepare_view_uniforms)
                .in_set(RenderSet::Prepare),
        )
        .add_systems(
            Render,
            (
                prepare_grid_shadow_bind_groups,
                prepare_grid_shadow_view_bind_group,
            )
                .in_set(RenderSet::PrepareBindGroups),
        )
        .add_systems(
            Render,
            (
                queue_grid_shadows,
                batch_and_prepare_render_phase::<GridShadow, MeshPipeline>,
            )
                .chain()
                .in_set(RenderSet::Queue),
        );

    let grid_shadow_pass_node = GridShadowPassNode::new(&mut render_app.world);
    let mut graph = render_app.world.resource_mut::<RenderGraph>();
    let draw_3d_graph = graph.get_sub_graph_mut(Core3d).unwrap();
    draw_3d_graph.add_node(GridShadowPassLabel, grid_shadow_pass_node);
    draw_3d_graph.add_node_edge(GridShadowPassLabel, Node3d::EndMainPass);
}
