use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use re_mutex::RwLock;
use slotmap::{Key, SlotMap};
use smallvec::SmallVec;

use super::resource::PoolError;

/// Frames an unused resource stays available for reuse before it is destroyed.
///
/// qaros-rerun: upstream keeps it for one frame. Headset eye views are drawn every second to
/// fourth viewer frame per user, so with one frame their full-resolution targets are destroyed
/// and created again for almost every eye frame (several milliseconds each at 1832x1920).
/// Measured at 1832x1920 with two users: pool creations per shown frame are 25 to 30 at 1,
/// 9 at 2, 7 to 9 at 3, about 6 at 4 and 5.3 to 5.9 at 8, with no measurable VRAM change.
pub const REUSE_GRACE_FRAMES: u64 = 4;

/// [`REUSE_GRACE_FRAMES`], or `QAROS_RERUN_POOL_GRACE` when set (for measuring).
fn reuse_grace_frames() -> u64 {
    static GRACE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *GRACE.get_or_init(|| {
        std::env::var("QAROS_RERUN_POOL_GRACE")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(REUSE_GRACE_FRAMES)
            .max(1)
    })
}

/// Resources created by any pool so far (qaros-rerun's frame timing log).
pub static POOL_RESOURCES_CREATED: AtomicU64 = AtomicU64::new(0);

pub trait DynamicResourcesDesc {
    fn resource_size_in_bytes(&self) -> u64;

    /// If true, a unused resources will be kept around for while and then re-used in following frames.
    /// If false, it will be destroyed on [`DynamicResourcePool::begin_frame`].
    fn allow_reuse(&self) -> bool;
}

pub struct DynamicResource<Handle, Desc: Debug, Res> {
    pub inner: Res,
    pub creation_desc: Desc,
    pub handle: Handle,
}

impl<Handle, Desc, Res> std::ops::Deref for DynamicResource<Handle, Desc, Res>
where
    Desc: Debug,
{
    type Target = Res;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

struct DynamicResourcePoolProtectedState<Handle: Key, Desc: Debug, Res> {
    /// All resources, including both resources that are in use and those that are marked as dead via [`Self::last_frame_deallocated`]
    ///
    /// We store any ref counted handle we give out in [`DynamicResourcePool::alloc`] here in order to keep it alive.
    /// Every [`DynamicResourcePool::begin_frame`] we check if the pool is now the only owner of the handle
    /// and if so mark it as deallocated.
    all_resources: SlotMap<Handle, Arc<DynamicResource<Handle, Desc, Res>>>,

    /// Resources no one but the pool holds, with the frame they were found unused in.
    /// They stay for [`REUSE_GRACE_FRAMES`] frames to allow reclamation.
    last_frame_deallocated: HashMap<Desc, SmallVec<[(Handle, u64); 4]>>,
}

/// Generic resource pool for all resources that have varying contents beyond their description.
///
/// Unlike in [`super::static_resource_pool::StaticResourcePool`], a resource can not be uniquely
/// identified by its description, as the same description can apply to several different resources.
pub(super) struct DynamicResourcePool<Handle: Key, Desc: Debug, Res> {
    state: RwLock<DynamicResourcePoolProtectedState<Handle, Desc, Res>>,

    current_frame_index: u64,
    total_resource_size_in_bytes: AtomicU64,
}

/// We cannot #derive(Default) as that would require Handle/Desc/Res to implement Default too.
impl<Handle: Key, Desc, Res> Default for DynamicResourcePool<Handle, Desc, Res>
where
    Desc: Debug,
{
    fn default() -> Self {
        Self {
            state: RwLock::new(DynamicResourcePoolProtectedState {
                all_resources: Default::default(),
                last_frame_deallocated: Default::default(),
            }),
            current_frame_index: Default::default(),
            total_resource_size_in_bytes: AtomicU64::new(0),
        }
    }
}

impl<Handle, Desc, Res> DynamicResourcePool<Handle, Desc, Res>
where
    Handle: Key,
    Desc: Clone + Eq + Hash + Debug + DynamicResourcesDesc,
{
    pub fn alloc<F: FnOnce(&Desc) -> Res>(
        &self,
        desc: &Desc,
        creation_func: F,
    ) -> Arc<DynamicResource<Handle, Desc, Res>> {
        re_tracing::profile_function!();
        let mut state = self.state.write();

        // First check if we can reclaim a resource we have around from a previous frame.
        if desc.allow_reuse()
            && let Entry::Occupied(mut entry) = state.last_frame_deallocated.entry(desc.clone())
        {
            re_log::trace!(?desc, "Reclaimed previously discarded resource");

            let (handle, _) = entry.get_mut().pop().unwrap();
            if entry.get().is_empty() {
                entry.remove();
            }

            return state.all_resources[handle].clone();
        }

        // Otherwise create a new resource
        re_log::trace!(?desc, "Allocated new resource");
        let inner_resource = {
            re_tracing::profile_scope!("creation_func");
            POOL_RESOURCES_CREATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            creation_func(desc)
        };
        self.total_resource_size_in_bytes.fetch_add(
            desc.resource_size_in_bytes(),
            std::sync::atomic::Ordering::Relaxed,
        );

        let handle = state.all_resources.insert_with_key(|handle| {
            Arc::new(DynamicResource {
                inner: inner_resource,
                creation_desc: desc.clone(),
                handle,
            })
        });

        state.all_resources[handle].clone()
    }

    pub fn get_from_handle(
        &self,
        handle: Handle,
    ) -> Result<Arc<DynamicResource<Handle, Desc, Res>>, PoolError> {
        self.state
            .read()
            .all_resources
            .get(handle)
            .cloned()
            .ok_or_else(|| {
                if handle.is_null() {
                    PoolError::NullHandle
                } else {
                    PoolError::ResourceNotAvailable
                }
            })
    }

    pub fn begin_frame(&mut self, frame_index: u64, mut on_destroy_resource: impl FnMut(&Res)) {
        re_tracing::profile_function!();
        self.current_frame_index = frame_index;
        let state = self.state.get_mut();

        let update_stats = |creation_desc: &Desc| {
            self.total_resource_size_in_bytes.fetch_sub(
                creation_desc.resource_size_in_bytes(),
                std::sync::atomic::Ordering::Relaxed,
            );
        };

        // Throw out any resources that we haven't reclaimed within the grace period.
        let mut expired: Vec<(Desc, Handle)> = Vec::new();
        state.last_frame_deallocated.retain(|desc, resources| {
            resources.retain(|(handle, deallocated_in_frame)| {
                let keep = frame_index.saturating_sub(*deallocated_in_frame) < reuse_grace_frames();
                if !keep {
                    expired.push((desc.clone(), *handle));
                }
                keep
            });
            !resources.is_empty()
        });
        for (desc, resource) in expired {
            let Some(removed_resource) = state.all_resources.remove(resource) else {
                re_log::debug_panic!(
                    "a resource was marked as destroyed that we no longer kept track of"
                );
                continue;
            };
            update_stats(&desc);
            on_destroy_resource(&removed_resource);
        }

        // If the strong count went down to 1, we must be the only ones holding on to handle.
        // If that's the case, push it to the re-use-next-frame list or discard.
        //
        // thread safety:
        // Since the count is pushed from 1 to 2 by `alloc`, it should not be possible to ever
        // get temporarily get back down to 1 without dropping the last user available copy of the Arc<Handle>.
        state.all_resources.retain(|_, resource| {
            if Arc::strong_count(resource) == 1 {
                if resource.creation_desc.allow_reuse() {
                    let parked = state
                        .last_frame_deallocated
                        .entry(resource.creation_desc.clone())
                        .or_default();
                    if !parked.iter().any(|(handle, _)| *handle == resource.handle) {
                        parked.push((resource.handle, frame_index));
                    }
                    true
                } else {
                    update_stats(&resource.creation_desc);
                    on_destroy_resource(&resource.inner);
                    false
                }
            } else {
                true
            }
        });
    }

    pub fn num_resources(&self) -> usize {
        self.state.read().all_resources.len()
    }

    pub fn total_resource_size_in_bytes(&self) -> u64 {
        self.total_resource_size_in_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl<Handle, Desc, Res> Drop for DynamicResourcePool<Handle, Desc, Res>
where
    Handle: Key,
    Desc: Debug,
{
    fn drop(&mut self) {
        // TODO(andreas): We're failing this check currently on re_viewer's shutdown.
        // This is primarily the case due to the way we store the render ctx itself and other things on egui-wgpu's paint callback resources
        // We shouldn't do this as it makes a whole lot of other things cumbersome. Instead, we should store it directly on the `App`
        // where we control the drop order.

        // for (_, alive_resource) in self.state.read().alive_resources.iter() {
        //     let alive_resource = alive_resource
        //         .as_ref()
        //         .expect("Alive resources should never be None");
        //     let ref_count = Arc::strong_count(alive_resource);

        //     assert!(ref_count == 1,
        //             "Resource has still {} owner(s) at the time of pool destruction. Description desc was {:?}",
        //             ref_count - 1,
        //             &alive_resource.creation_desc);
        // }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::{DynamicResourcePool, DynamicResourcesDesc, REUSE_GRACE_FRAMES};

    slotmap::new_key_type! { pub struct ConcreteHandle; }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub struct ConcreteResourceDesc(u32);

    impl DynamicResourcesDesc for ConcreteResourceDesc {
        fn resource_size_in_bytes(&self) -> u64 {
            1
        }

        fn allow_reuse(&self) -> bool {
            true
        }
    }

    thread_local! {
        static DROP_COUNTER: Cell<usize> = const { Cell::new(0) };
    }

    #[derive(Debug)]
    pub struct ConcreteResource;

    impl Drop for ConcreteResource {
        fn drop(&mut self) {
            DROP_COUNTER.with(|c| {
                c.set(c.get() + 1);
            });
        }
    }

    type Pool = DynamicResourcePool<ConcreteHandle, ConcreteResourceDesc, ConcreteResource>;

    #[test]
    fn resource_alloc_and_reuse() {
        let mut pool = Pool::default();

        let initial_resource_descs = [0, 0, 1, 2, 2, 3];

        // Alloc on a new pool always returns a new resource.
        allocate_resources(&initial_resource_descs, &pool, true);

        // After frame maintenance we get used resources.
        // Still, no resources were dropped.
        {
            let drop_counter_before = DROP_COUNTER.with(|c| c.get());
            let mut called_destroy = false;
            pool.begin_frame(1, |_| called_destroy = true);

            assert!(!called_destroy);
            assert_eq!(drop_counter_before, DROP_COUNTER.with(|c| c.get()),);
        }

        // Allocate the same resources again, this should *not* create any new resources.
        allocate_resources(&initial_resource_descs, &pool, false);
        // Doing it again, it will again create resources.
        allocate_resources(&initial_resource_descs, &pool, true);

        // Unused resources are dropped once the grace period has passed.
        {
            let drop_counter_before = DROP_COUNTER.with(|c| c.get());
            let mut called_destroy = false;
            for frame_index in 2..2 + REUSE_GRACE_FRAMES {
                pool.begin_frame(frame_index, |_| called_destroy = true);
                assert!(!called_destroy);
            }
            pool.begin_frame(2 + REUSE_GRACE_FRAMES, |_| called_destroy = true);
            assert!(called_destroy);
            let drop_counter_now = DROP_COUNTER.with(|c| c.get());
            assert_eq!(
                drop_counter_before + initial_resource_descs.len() * 2,
                drop_counter_now
            );
            assert_eq!(pool.total_resource_size_in_bytes(), 0);
        }

        // Holding on to the resource avoids both re-use and dropping.
        {
            let drop_counter_before = DROP_COUNTER.with(|c| c.get());
            let resource0 = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);
            let resource1 = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);
            assert_ne!(resource0.handle, resource1.handle);
            drop(resource1);

            let mut called_destroy = false;
            let parked_in = 3 + REUSE_GRACE_FRAMES;
            for frame_index in parked_in..parked_in + REUSE_GRACE_FRAMES {
                pool.begin_frame(frame_index, |_| called_destroy = true);
                assert!(!called_destroy);
            }
            assert_eq!(drop_counter_before, DROP_COUNTER.with(|c| c.get()),);
            pool.begin_frame(parked_in + REUSE_GRACE_FRAMES, |_| called_destroy = true);
            assert!(called_destroy);
            assert_eq!(drop_counter_before + 1, DROP_COUNTER.with(|c| c.get()),);
        }
    }

    // Two resources have two different handles.
    #[test]
    fn individual_handles() {
        let mut pool = Pool::default();
        let res0 = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);
        let res1 = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);
        assert_ne!(res0.handle, res1.handle);
        pool.begin_frame(1234, |_| {});
    }

    // A resource unused for a few frames is still reused, not created again.
    #[test]
    fn resource_is_reused_within_the_grace_period() {
        let mut pool = Pool::default();
        let resource = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);
        let handle = resource.handle;
        drop(resource);
        for frame_index in 1..REUSE_GRACE_FRAMES {
            pool.begin_frame(frame_index, |_| {});
        }
        let created = Cell::new(false);
        let reused = pool.alloc(&ConcreteResourceDesc(0), |_| {
            created.set(true);
            ConcreteResource
        });
        assert!(!created.get());
        assert_eq!(handle, reused.handle);
    }

    // A resource gets the same handle when re-used.
    // (important for BindGroup re-use!)
    #[test]
    fn handle_unchanged_on_reuse() {
        let mut pool = Pool::default();
        let res0 = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);
        let handle0 = res0.handle;
        drop(res0);
        pool.begin_frame(1234, |_| {});
        let res1 = pool.alloc(&ConcreteResourceDesc(0), |_| ConcreteResource);

        assert_eq!(handle0, res1.handle);
        pool.begin_frame(1235, |_| {});
    }

    fn allocate_resources(
        descs: &[u32],
        pool: &DynamicResourcePool<ConcreteHandle, ConcreteResourceDesc, ConcreteResource>,
        expect_allocation: bool,
    ) {
        let drop_counter_before = DROP_COUNTER.with(|c| c.get());
        let byte_count_before = pool.total_resource_size_in_bytes();
        for &desc in descs {
            // Previous loop iteration didn't drop Resources despite dropping a handle.
            assert_eq!(drop_counter_before, DROP_COUNTER.with(|c| c.get()));

            let new_resource_created = Cell::new(false);
            let resource = pool.alloc(&ConcreteResourceDesc(desc), |_| {
                new_resource_created.set(true);
                ConcreteResource
            });
            assert_eq!(new_resource_created.get(), expect_allocation);

            // Resource pool keeps the handle alive, but otherwise we're the only owners.
            assert_eq!(Arc::strong_count(&resource), 2);
        }

        if expect_allocation {
            assert_eq!(
                byte_count_before
                    + descs
                        .iter()
                        .map(|d| ConcreteResourceDesc(*d).resource_size_in_bytes())
                        .sum::<u64>(),
                pool.total_resource_size_in_bytes()
            );
        } else {
            assert_eq!(byte_count_before, pool.total_resource_size_in_bytes());
        }
    }
}
