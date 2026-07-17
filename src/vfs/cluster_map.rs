//! Cluster geometry trait used by [`super::stream::StreamFile`].

/// Result of [`ClusterMap::extend`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExtendResult {
    /// First cluster of the stream.
    pub first_cluster: u32,
    /// Last cluster of the (possibly grown) chain.
    pub tail: u32,
}

/// Result of [`ClusterMap::free_tail`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct FreeTailResult {
    /// First cluster of the stream (`0` if fully freed).
    pub first_cluster: u32,
}

/// Format-specific cluster allocation map for one open stream.
pub(crate) trait ClusterMap {
    type Error;

    /// Next cluster after `cluster`, or `None` at end-of-chain / past run.
    fn next(&self, cluster: u32) -> Result<Option<u32>, Self::Error>;

    /// Grow allocation so at least `needed_allocated_len` bytes are covered.
    fn extend(
        &mut self,
        first: Option<u32>,
        tail: Option<u32>,
        needed_allocated_len: u64,
    ) -> Result<ExtendResult, Self::Error>;

    /// Free clusters past the prefix that holds `keep_len` bytes.
    fn free_tail(&mut self, first: u32, keep_len: u64) -> Result<FreeTailResult, Self::Error>;

    fn cluster_size(&self) -> u32;
    fn cluster_to_offset(&self, cluster: u32) -> u64;
    fn no_fat_chain(&self) -> bool;
    fn allocated_len(&self) -> u64;
}
