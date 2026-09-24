use camino::{Utf8Path, Utf8PathBuf};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hint::black_box;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use gentoo_core::Arch;
use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{PortagePackage, UseLayer};
use portage_metadata::CacheEntry;
use portage_repo::{AcceptSet, LicenseGroupRegistry};
use portage_resolve::Roots;
use portage_resolve::bdepend_trim::{TrimCtx, trim_within_run_bdepend};
use portage_resolve::depend_trim::trim_sysroot_satisfied_depend;
use portage_resolve::force_mask::ForceMask;
use portage_resolve::installed::BrootSnapshot;
use portage_resolve::repo::{AcceptKeywords, AcceptOverlay, RepoData, ResolvePolicy};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct Fixture {
    root: Utf8PathBuf,
    data: RepoData,
    broot_snapshot: BrootSnapshot,
    order: Vec<(PortagePackage, Version)>,
    root_cpns: HashSet<Cpn>,
    reinstall_cpns: HashSet<Cpn>,
    accept_keywords: AcceptKeywords,
    accept_licenses: AcceptOverlay,
    force_mask: ForceMask,
}

impl Fixture {
    fn new(plan_size: usize, vdb_size: usize) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_nanos();
        let root = Utf8PathBuf::from_path_buf(
            std::env::temp_dir().join(format!("portage-bench-trim-{}-{stamp}", std::process::id())),
        )
        .unwrap();
        let vdb = root.join("var/db/pkg");
        fs::create_dir_all(vdb.join("app-misc")).unwrap();
        for i in 0..vdb_size {
            let package = vdb.join("app-misc").join(format!("host{i:05}-1.0"));
            fs::create_dir_all(&package).unwrap();
            fs::write(package.join("SLOT"), "0\n").unwrap();
        }

        let mut cpns = Vec::with_capacity(plan_size);
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut order = Vec::with_capacity(plan_size);
        for i in 0..plan_size {
            let cpn = Cpn::parse(&format!("app-misc/pkg{i:05}")).unwrap();
            let cpv = Cpv::parse(&format!("app-misc/pkg{i:05}-1.0")).unwrap();
            let next = (i + 1) % plan_size;
            let text = format!(
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=trim benchmark\nBDEPEND=app-misc/pkg{next:05}\n"
            );
            let cache = CacheEntry::parse(&text).unwrap();
            cpns.push(cpn);
            versions.entry(cpn).or_default().push((cpv.clone(), cache));
            order.push((PortagePackage::unslotted(cpn), cpv.version));
        }

        let roots = Roots::for_test(root.as_str());
        let broot_snapshot = BrootSnapshot::load(&roots);
        let data = RepoData::from_parts(
            cpns,
            versions,
            "bench".into(),
            HashMap::new(),
            HashMap::new(),
        );
        let accept_keywords = AcceptKeywords::from_global(&Arch::intern("amd64"), &["amd64"]);
        let accept_licenses = AcceptOverlay::new(
            AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
            Vec::new(),
        );
        Self {
            root,
            data,
            broot_snapshot,
            order,
            root_cpns: HashSet::new(),
            reinstall_cpns: HashSet::new(),
            accept_keywords,
            accept_licenses,
            force_mask: ForceMask::default(),
        }
    }

    fn ctx(&self) -> TrimCtx<'_> {
        TrimCtx {
            broot_snapshot: &self.broot_snapshot,
            data: &self.data,
            policy: ResolvePolicy {
                accept_keywords: &self.accept_keywords,
                package_mask: &portage_resolve::repo::PolicyMaskList::EMPTY,
                package_unmask: &portage_resolve::repo::PolicyMaskList::EMPTY,
                accept_licenses: &self.accept_licenses,
                accept_properties: &self.accept_licenses,
                accept_restrict: &self.accept_licenses,
                defaults: empty_layer(),
                conf: empty_layer(),
                env_use: empty_layer(),
                package_use: &[],
                profile_package_use: &[],
                force_mask: &self.force_mask,
                facts: None,
            },
            root_cpns: &self.root_cpns,
            reinstall_cpns: &self.reinstall_cpns,
        }
    }

    fn target(&self) -> &Utf8Path {
        &self.root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn empty_layer() -> &'static UseLayer {
    static EMPTY: OnceLock<UseLayer> = OnceLock::new();
    EMPTY.get_or_init(UseLayer::default)
}

fn bench_trim(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolver/trim");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    for (plan_size, vdb_size) in [(16usize, 16usize), (32, 32), (64, 64), (128, 128)] {
        let fixture = Fixture::new(plan_size, vdb_size);
        let ctx = fixture.ctx();
        let full_order = fixture.order.clone();
        let label = format!("{plan_size}x{vdb_size}");
        eprintln!(
            "trim fixture {label}: {} package pairs, {plan_size} evaluated cache entries, {vdb_size} VDB entries",
            plan_size * plan_size.saturating_sub(1) / 2
        );

        group.bench_with_input(BenchmarkId::new("bdepend", &label), &label, |b, _| {
            b.iter_custom(|iterations| {
                ALLOCATIONS.store(0, Ordering::Relaxed);
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(trim_within_run_bdepend(
                        fixture.order.clone(),
                        &full_order,
                        true,
                        &ctx,
                    ));
                }
                let elapsed = start.elapsed();
                let allocations = ALLOCATIONS.load(Ordering::Relaxed);
                if iterations >= 1024 {
                    eprintln!(
                        "trim fixture {label} bdepend: {allocations} allocations / {iterations} iterations"
                    );
                }
                elapsed
            })
        });

        let sysroot = fixture.target().join("sysroot");
        let target = fixture.target().join("target");
        group.bench_with_input(BenchmarkId::new("depend", &label), &label, |b, _| {
            b.iter_custom(|iterations| {
                ALLOCATIONS.store(0, Ordering::Relaxed);
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(trim_sysroot_satisfied_depend(
                        fixture.order.clone(),
                        Some(&sysroot),
                        &target,
                        &ctx,
                    ));
                }
                let elapsed = start.elapsed();
                let allocations = ALLOCATIONS.load(Ordering::Relaxed);
                if iterations >= 1024 {
                    eprintln!(
                        "trim fixture {label} depend: {allocations} allocations / {iterations} iterations"
                    );
                }
                elapsed
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_trim);
criterion_main!(benches);
