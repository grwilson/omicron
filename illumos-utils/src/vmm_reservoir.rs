// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Wrappers around VMM Reservoir controls

use omicron_common::api::external::ByteCount;

#[allow(non_upper_case_globals)]
const MiB: u64 = 1024 * 1024;
#[allow(non_upper_case_globals)]
const GiB: u64 = 1024 * 1024 * 1024;

/// The control plane's required alignment for a reservoir size request.
//
// The ioctl interface for resizing the reservoir only requires rounding the
// requested size to PAGESIZE. But we choose a larger value here of 2MiB to
// better enable large pages for guests in the future: Currently, the reservoir
// does not have support for large pages, but choosing a size that aligns with
// the minimum large page size on x86 (2 MiB) will simplify things later.
///
pub const RESERVOIR_SZ_ALIGN: u64 = 2 * MiB;

// Chunk size to request when resizing the reservoir. It's okay if this value is
// greater than the requested size.
const RESERVOIR_CHUNK_SZ: usize = GiB as usize;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Reservoir size must be aligned to 2 MiB: {0}")]
    InvalidSize(ByteCount),

    #[error("Failed to open vmm ctl fd")]
    VmCtlOpen(#[source] std::io::Error),

    #[error("Failed to query reservoir")]
    ReservoirQuery(#[source] std::io::Error),

    #[error("Failed to resize reservoir")]
    ReservoirError {
        #[source]
        error: std::io::Error,
        current_size: Option<ByteCount>,
    },
}

/// Controls the size of the memory reservoir
pub struct ReservoirControl {}

impl ReservoirControl {
    /// Sets the reservoir to a particular size.
    ///
    /// The size must be aligned on RESERVOIR_SZ_ALIGN.
    pub fn set(size: ByteCount) -> Result<(), Error> {
        if !reservoir_size_is_aligned(size.to_bytes()) {
            return Err(Error::InvalidSize(size));
        }

        let ctl =
            bhyve_api::VmmCtlFd::open().map_err(|e| Error::VmCtlOpen(e))?;
        ctl.reservoir_resize(
            size.to_bytes().try_into().map_err(|_| Error::InvalidSize(size))?,
            RESERVOIR_CHUNK_SZ,
        )
        .map_err(|e| Error::ReservoirError {
            error: e.into(),
            current_size: ctl.reservoir_query().ok().and_then(|v| {
                ByteCount::try_from((v.vrq_free_sz + v.vrq_alloc_sz) as u64)
                    .ok()
            }),
        })?;

        Ok(())
    }
}

// `VMM_RESV_QUERY2` and `struct vmm_resv_query2`, from illumos
// `sys/vmm_dev.h`.
//
// TODO: Replace these with the definitions from `bhyve_api` once propolis
// exposes them.
const VMM_RESV_QUERY2: i32 =
    ((b'V' as i32) << 16) | ((b'M' as i32) << 8) | 0x12;

#[repr(C)]
#[derive(Default)]
// Only `vrq_rawmem_total_sz` is read here, but the layout must match the
// kernel's struct exactly since the kernel copies out the whole thing.
#[allow(dead_code)]
struct VmmResvQuery2 {
    vrq_free_sz: usize,
    vrq_alloc_sz: usize,
    vrq_alloc_transient_sz: usize,
    vrq_limit: usize,
    vrq_rawmem_total_sz: usize,
    vrq_rawmem_free_sz: usize,
}

/// Returns the total size, in bytes, of the kernel's rawmem pool.
///
/// rawmem is physical memory the kernel withholds from `page_t` management at
/// boot for the VMM reservoir to draw from. Because it has no `page_t`s, it is
/// not counted by `sysconf(_SC_PHYS_PAGES)`. Returns 0 on kernels without
/// rawmem support.
pub fn rawmem_total_bytes() -> Result<u64, Error> {
    let ctl = bhyve_api::VmmCtlFd::open().map_err(|e| Error::VmCtlOpen(e))?;
    let mut query = VmmResvQuery2::default();

    // Safety: `VmmResvQuery2` matches the kernel's `struct vmm_resv_query2`,
    // which is all that VMM_RESV_QUERY2 copies out.
    match unsafe { ctl.ioctl(VMM_RESV_QUERY2, &mut query) } {
        Ok(_) => Ok(query.vrq_rawmem_total_sz as u64),
        // Kernels which predate rawmem don't recognize this ioctl.
        Err(e) if e.raw_os_error() == Some(libc::ENOTTY) => Ok(0),
        Err(e) => Err(Error::ReservoirQuery(e)),
    }
}

pub fn align_reservoir_size(size_bytes: u64) -> u64 {
    size_bytes - (size_bytes % RESERVOIR_SZ_ALIGN)
}

pub fn reservoir_size_is_aligned(size_bytes: u64) -> bool {
    size_bytes.is_multiple_of(RESERVOIR_SZ_ALIGN)
}
