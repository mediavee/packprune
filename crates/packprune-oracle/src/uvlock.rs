//! Where a uv-locked block state variant sends a face's UVs, as the 26.2 client bakes it:
//! `FaceBakery.bakeVertex` applies `ModelState.inverseFaceTransformation(face)` to each corner around the middle of
//! the texture, and `BlockModelRotation` builds it from `BlockMath.getFaceTransformation` of the variant's rotation
//! (`Quadrant.fromXYZAngles`). Every matrix is a rotation by quarter turns, so integers hold it exactly.

type Matrix = [[i32; 3]; 3];

/// `Direction` order, which the client's face map iterates in.
pub(crate) const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
const NORMALS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];
const IDENTITY: Matrix = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];
/// `OctahedralGroup.BLOCK_ROT_*_90`: `ROT_90_*_NEG`, a sign flip times a permutation.
const BLOCK_ROT_X_90: Matrix = [[1, 0, 0], [0, 0, 1], [0, -1, 0]];
const BLOCK_ROT_Y_90: Matrix = [[0, 0, -1], [0, 1, 0], [1, 0, 0]];
const BLOCK_ROT_Z_90: Matrix = [[0, 1, 0], [-1, 0, 0], [0, 0, 1]];

/// The 2x2 map `[[a, b], [c, d]]` with `u' = 8 + a (u - 8) + b (v - 8)` and `v' = 8 + c (u - 8) + d (v - 8)`, in
/// sixteenths, for a face of `direction` under rotations of `x`, `y` and `z` quarter turns.
pub(crate) fn map(x: i32, y: i32, z: i32, direction: usize) -> [[i32; 2]; 2] {
    let rotation = multiply(
        &power(&BLOCK_ROT_Z_90, z),
        &multiply(&power(&BLOCK_ROT_Y_90, y), &power(&BLOCK_ROT_X_90, x)),
    );
    if rotation == IDENTITY {
        return [[1, 0], [0, 1]];
    }
    // BlockMath.getFaceTransformation: the face's local frame to world, rotated, back to the frame of the side it lands on.
    let face_action = multiply(&rotation, &local_to_global(direction));
    let normal = apply(&face_action, [0, 0, 1]);
    // The side the rotated face's normal points to.
    let side = NORMALS.iter().rposition(|n| *n == normal).unwrap_or(0);
    let face = multiply(&transpose(&local_to_global(side)), &face_action);
    let inverse = transpose(&face);
    [
        [inverse[0][0], inverse[0][1]],
        [inverse[1][0], inverse[1][1]],
    ]
}

/// `BlockMath.VANILLA_UV_TRANSFORM_LOCAL_TO_GLOBAL`, by [`DIRECTIONS`] index.
fn local_to_global(direction: usize) -> Matrix {
    match direction {
        0 => rotate_x(1),
        1 => rotate_x(-1),
        2 => rotate_y(2),
        3 => IDENTITY,
        4 => rotate_y(-1),
        _ => rotate_y(1),
    }
}

/// JOML's `rotateX` by `quarters` quarter turns.
fn rotate_x(quarters: i32) -> Matrix {
    let (c, s) = (cos(quarters), sin(quarters));
    [[1, 0, 0], [0, c, -s], [0, s, c]]
}

fn rotate_y(quarters: i32) -> Matrix {
    let (c, s) = (cos(quarters), sin(quarters));
    [[c, 0, s], [0, 1, 0], [-s, 0, c]]
}

fn cos(quarters: i32) -> i32 {
    [1, 0, -1, 0][quarters.rem_euclid(4) as usize]
}

fn sin(quarters: i32) -> i32 {
    [0, 1, 0, -1][quarters.rem_euclid(4) as usize]
}

fn power(matrix: &Matrix, times: i32) -> Matrix {
    (0..times.rem_euclid(4)).fold(IDENTITY, |out, _| multiply(matrix, &out))
}

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn apply(matrix: &Matrix, vector: [i32; 3]) -> [i32; 3] {
    let mut out = [0; 3];
    for (i, cell) in out.iter_mut().enumerate() {
        *cell = (0..3).map(|k| matrix[i][k] * vector[k]).sum();
    }
    out
}

fn transpose(matrix: &Matrix) -> Matrix {
    let mut out = [[0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = matrix[j][i];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::map;

    #[test]
    fn no_rotation_is_the_identity_and_y_90_turns_the_up_face() {
        assert_eq!(map(0, 0, 0, 1), [[1, 0], [0, 1]]);
        assert_eq!(map(0, 4, 0, 1), [[1, 0], [0, 1]]);
        // y 90 moves the up face's [0, 0, 4, 4] to [12, 0, 16, 4].
        let m = map(0, 1, 0, 1);
        let corner = |u: i32, v: i32| {
            (
                8 + m[0][0] * (u - 8) + m[0][1] * (v - 8),
                8 + m[1][0] * (u - 8) + m[1][1] * (v - 8),
            )
        };
        let corners = [corner(0, 0), corner(4, 4)];
        let xs = (
            corners[0].0.min(corners[1].0),
            corners[0].0.max(corners[1].0),
        );
        let ys = (
            corners[0].1.min(corners[1].1),
            corners[0].1.max(corners[1].1),
        );
        assert_eq!((xs, ys), ((12, 16), (0, 4)));
    }
}
