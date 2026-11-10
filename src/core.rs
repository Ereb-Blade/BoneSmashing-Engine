#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaterialId(pub usize);

pub const MESH_CUBE: MeshId = MeshId(0);
pub const MESH_PLANE: MeshId = MeshId(1);
pub const MESH_SPHERE: MeshId = MeshId(2);

pub const MATERIAL_DEFAULT: MaterialId = MaterialId(0);
pub const MATERIAL_RED_SHINY: MaterialId = MaterialId(1);
pub const MATERIAL_CHROME: MaterialId = MaterialId(2);
pub const MATERIAL_SILVER: MaterialId = MaterialId(3);
pub const MATERIAL_GOLD: MaterialId = MaterialId(4);
pub const MATERIAL_GLASS: MaterialId = MaterialId(5);
