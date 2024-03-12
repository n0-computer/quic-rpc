use std::{marker::PhantomData, sync::Arc};

use crate::Service;

/// Convert requests and responses between an outer and an inner service.
///
/// An "outer" service has request and response enums which wrap the requests and responses of an
/// "inner" service. This trait is implemented on the [`Mapper`] and [`ChainedMapper`] structs
/// to convert the requests and responses between the outer and inner services.
pub trait MapService<SOuter: Service, SInner: Service>:
    std::fmt::Debug + Send + Sync + 'static
{
    /// Convert an inner request into the outer request.
    fn req_into_outer(&self, req: SInner::Req) -> SOuter::Req;

    /// Convert an inner response into the outer response.
    fn res_into_outer(&self, res: SInner::Res) -> SOuter::Res;

    /// Try to convert the outer request into the inner request.
    ///
    /// Returns an error if the request is not of the variant of the inner service.
    fn req_try_into_inner(&self, req: SOuter::Req) -> Result<SInner::Req, ()>;

    /// Try to convert the outer response into the inner request.
    ///
    /// Returns an error if the response is not of the variant of the inner service.
    fn res_try_into_inner(&self, res: SOuter::Res) -> Result<SInner::Res, ()>;
}

/// Zero-sized struct to map between two services.
#[derive(Debug)]
pub struct Mapper<SOuter, SInner>(PhantomData<SOuter>, PhantomData<SInner>);

impl<SOuter, SInner> Mapper<SOuter, SInner>
where
    SOuter: Service,
    SInner: Service,
    SInner::Req: Into<SOuter::Req> + TryFrom<SOuter::Req>,
    SInner::Res: Into<SOuter::Res> + TryFrom<SOuter::Res>,
{
    /// Create a new mapper between `SOuter` and `SInner` services.
    ///
    /// This method is availalbe if the required bounds to convert between the outer and inner
    /// request and response enums are met:
    /// `SInner::Req: Into<SOuter::Req> + TryFrom<SOuter::Req>`
    /// `SInner::Res: Into<SOuter::Res> + TryFrom<SOuter::Res>`
    pub fn new() -> Self {
        Self(PhantomData, PhantomData)
    }
}

impl<SOuter, SInner> MapService<SOuter, SInner> for Mapper<SOuter, SInner>
where
    SOuter: Service,
    SInner: Service,
    SInner::Req: Into<SOuter::Req> + TryFrom<SOuter::Req>,
    SInner::Res: Into<SOuter::Res> + TryFrom<SOuter::Res>,
{
    fn req_into_outer(&self, req: SInner::Req) -> SOuter::Req {
        req.into()
    }

    fn res_into_outer(&self, res: SInner::Res) -> SOuter::Res {
        res.into()
    }

    fn req_try_into_inner(&self, req: SOuter::Req) -> Result<SInner::Req, ()> {
        req.try_into().map_err(|_| ())
    }

    fn res_try_into_inner(&self, res: SOuter::Res) -> Result<SInner::Res, ()> {
        res.try_into().map_err(|_| ())
    }
}

/// Map between an outer and an inner service with any number of intermediate services.
///
/// This uses an `Arc<dyn MapService>` to contain an unlimited chain of [`Mapper`]s.
#[derive(Debug)]
pub struct ChainedMapper<SOuter, SMid, SInner>
where
    SOuter: Service,
    SMid: Service,
    SInner: Service,
    SInner::Req: Into<SMid::Req> + TryFrom<SMid::Req>,
{
    map1: Arc<dyn MapService<SOuter, SMid>>,
    map2: Mapper<SMid, SInner>,
}

impl<SOuter, SMid, SInner> ChainedMapper<SOuter, SMid, SInner>
where
    SOuter: Service,
    SMid: Service,
    SInner: Service,
    SInner::Req: Into<SMid::Req> + TryFrom<SMid::Req>,
    SInner::Res: Into<SMid::Res> + TryFrom<SMid::Res>,
{
    /// Create a new [`ChainedMapper`] by appending a service `SInner` to the existing `dyn
    /// MapService<SOuter, SMid>`.
    ///
    /// Usage example:
    /// ```ignore
    /// // S1 is a Service and impls the Into and TryFrom traits to map to S2
    /// // S2 is a Service and impls the Into and TryFrom traits to map to S3
    /// // S3 is also a Service
    ///
    /// let mapper: Mapper<S1, S2> = Mapper::new();
    /// let mapper: Arc<dyn MapService<S1, S2>> = Arc::new(mapper);
    /// let chained_mapper: ChainedMapper<S1, S2, S3> = ChainedMapper::new(mapper);
    /// ```
    pub fn new(map1: Arc<dyn MapService<SOuter, SMid>>) -> Self {
        Self {
            map1,
            map2: Mapper::new(),
        }
    }
}

impl<SOuter, SMid, SInner> MapService<SOuter, SInner> for ChainedMapper<SOuter, SMid, SInner>
where
    SOuter: Service,
    SMid: Service,
    SInner: Service,
    SInner::Req: Into<SMid::Req> + TryFrom<SMid::Req>,
    SInner::Res: Into<SMid::Res> + TryFrom<SMid::Res>,
{
    fn req_into_outer(&self, req: SInner::Req) -> SOuter::Req {
        let req = self.map2.req_into_outer(req);
        self.map1.req_into_outer(req)
    }
    fn res_into_outer(&self, res: SInner::Res) -> SOuter::Res {
        let res = self.map2.res_into_outer(res);
        self.map1.res_into_outer(res)
    }
    fn req_try_into_inner(&self, req: SOuter::Req) -> Result<SInner::Req, ()> {
        let req = self.map1.req_try_into_inner(req)?;
        self.map2.req_try_into_inner(req)
    }

    fn res_try_into_inner(&self, res: SOuter::Res) -> Result<SInner::Res, ()> {
        let res = self.map1.res_try_into_inner(res)?;
        self.map2.res_try_into_inner(res)
    }
}
