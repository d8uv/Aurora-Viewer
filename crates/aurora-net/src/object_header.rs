//! The common prefix of a compressed object update.

use glam::Vec3;
use uuid::Uuid;

pub(crate) struct CompressedHeader {
    pub full_id: Uuid,
    pub local_id: u32,
    pub pcode: u8,
    pub state: u8,
    #[cfg(feature = "viewer")]
    pub crc: u32,
    #[cfg(feature = "viewer")]
    pub material: u8,
    #[cfg(feature = "viewer")]
    pub click_action: u8,
    #[cfg(feature = "viewer")]
    pub scale: Vec3,
    #[cfg(feature = "viewer")]
    pub position: Vec3,
    #[cfg(feature = "viewer")]
    pub rotation: Vec3,
    pub flags: u32,
    #[cfg(feature = "viewer")]
    pub owner_id: Uuid,
    #[cfg(feature = "viewer")]
    pub angular_velocity: Vec3,
    pub parent_id: u32,
    pub offset: usize,
}

pub(crate) fn compressed_header(data: &[u8]) -> Option<CompressedHeader> {
    fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
    }
    fn vec3_at(data: &[u8], offset: usize) -> Option<Vec3> {
        Some(Vec3::new(
            f32::from_bits(u32_at(data, offset)?),
            f32::from_bits(u32_at(data, offset + 4)?),
            f32::from_bits(u32_at(data, offset + 8)?),
        ))
    }
    data.get(..84)?;
    let flags = u32_at(data, 64)?;
    let mut offset = 84;
    let _angular_velocity = if flags & 0x80 != 0 {
        let value = vec3_at(data, offset)?;
        offset += 12;
        value
    } else {
        Vec3::ZERO
    };
    let parent_id = if flags & 0x20 != 0 {
        let value = u32_at(data, offset)?;
        offset += 4;
        value
    } else {
        0
    };
    Some(CompressedHeader {
        full_id: Uuid::from_slice(data.get(..16)?).ok()?,
        local_id: u32_at(data, 16)?,
        pcode: *data.get(20)?,
        state: *data.get(21)?,
        #[cfg(feature = "viewer")]
        crc: u32_at(data, 22)?,
        #[cfg(feature = "viewer")]
        material: *data.get(26)?,
        #[cfg(feature = "viewer")]
        click_action: *data.get(27)?,
        #[cfg(feature = "viewer")]
        scale: vec3_at(data, 28)?,
        #[cfg(feature = "viewer")]
        position: vec3_at(data, 40)?,
        #[cfg(feature = "viewer")]
        rotation: vec3_at(data, 52)?,
        flags,
        #[cfg(feature = "viewer")]
        owner_id: Uuid::from_slice(data.get(68..84)?).ok()?,
        #[cfg(feature = "viewer")]
        angular_velocity: _angular_velocity,
        parent_id,
        offset,
    })
}

/// Read only the name-value block; the terminal does not need geometry.
#[cfg(feature = "chat")]
pub(crate) fn compressed_name_values<'a>(data: &'a [u8], header: &CompressedHeader) -> Option<&'a str> {
    struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
    }
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let end = self.pos.checked_add(n)?;
            let bytes = self.data.get(self.pos..end)?;
            self.pos = end;
            Some(bytes)
        }
        fn u32(&mut self) -> Option<usize> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize)
        }
        fn binary(&mut self) -> Option<()> {
            let len = self.u32()?;
            self.take(len)?;
            Some(())
        }
        fn cstr(&mut self) -> Option<&'a [u8]> {
            let rest = self.data.get(self.pos..)?;
            let end = rest.iter().position(|byte| *byte == 0)?;
            let value = self.take(end)?;
            self.take(1)?;
            Some(value)
        }
    }
    let mut r = Reader { data, pos: header.offset };
    let flags = header.flags;
    if flags & 0x2 != 0 {
        r.take(1)?;
    } else if flags & 0x1 != 0 {
        r.take(4)?;
        r.binary()?;
    }
    if flags & 0x4 != 0 {
        r.cstr()?;
        r.take(4)?;
    }
    if flags & 0x200 != 0 {
        r.cstr()?;
    }
    if flags & 0x8 != 0 {
        r.take(86)?;
    }
    let count = *r.take(1)?.first()?;
    for _ in 0..count {
        r.take(2)?;
        r.binary()?;
    }
    if flags & 0x10 != 0 {
        r.take(25)?;
    }
    if flags & 0x100 == 0 {
        return Some("");
    }
    std::str::from_utf8(r.cstr()?).ok()
}
