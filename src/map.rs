use std::{marker::PhantomData, sync::Arc};

use crate::Service;

pub trait MapService<S1: Service, S2: Service>: std::fmt::Debug + Send + Sync + 'static {
    fn req_up(&self, req: S2::Req) -> S1::Req;
    fn res_up(&self, res: S2::Res) -> S1::Res;
    fn try_req_down(&self, req: S1::Req) -> Result<S2::Req, ()>;
    fn try_res_down(&self, res: S1::Res) -> Result<S2::Res, ()>;
}

#[derive(Debug)]
pub struct Mapper<S1, S2> {
    s1: PhantomData<S1>,
    s2: PhantomData<S2>,
}

impl<S1, S2> Mapper<S1, S2>
where
    S1: Service,
    S2: Service,
    S2::Req: Into<S1::Req> + TryFrom<S1::Req>,
    S2::Res: Into<S1::Res> + TryFrom<S1::Res>,
{
    pub fn new() -> Self {
        Self {
            s1: PhantomData,
            s2: PhantomData,
        }
    }
}

impl<S1, S2> MapService<S1, S2> for Mapper<S1, S2>
where
    S1: Service,
    S2: Service,
    S2::Req: Into<S1::Req> + TryFrom<S1::Req>,
    S2::Res: Into<S1::Res> + TryFrom<S1::Res>,
{
    fn req_up(&self, req: S2::Req) -> S1::Req {
        (req.into()).into()
    }

    fn res_up(&self, res: S2::Res) -> S1::Res {
        (res.into()).into()
    }

    fn try_req_down(&self, req: S1::Req) -> Result<S2::Req, ()> {
        req.try_into().map_err(|_| ())
    }

    fn try_res_down(&self, res: S1::Res) -> Result<S2::Res, ()> {
        res.try_into().map_err(|_| ())
    }
}

#[derive(Debug)]
pub struct ChainedMapper<S1, S2, S3>
where
    S1: Service,
    S2: Service,
    S3: Service,
    S3::Req: Into<S2::Req> + TryFrom<S2::Req>,
{
    m1: Arc<dyn MapService<S1, S2>>,
    m2: Mapper<S2, S3>,
}

impl<S1, S2, S3> ChainedMapper<S1, S2, S3>
where
    S1: Service,
    S2: Service,
    S3: Service,
    S3::Req: Into<S2::Req> + TryFrom<S2::Req>,
    S3::Res: Into<S2::Res> + TryFrom<S2::Res>,
{
    pub fn new(m1: Arc<dyn MapService<S1, S2>>) -> Self {
        Self {
            m1,
            m2: Mapper::new(),
        }
    }
}

impl<S1, S2, S3> MapService<S1, S3> for ChainedMapper<S1, S2, S3>
where
    S1: Service,
    S2: Service,
    S3: Service,
    S3::Req: Into<S2::Req> + TryFrom<S2::Req>,
    S3::Res: Into<S2::Res> + TryFrom<S2::Res>,
{
    fn req_up(&self, req: S3::Req) -> S1::Req {
        let req = self.m2.req_up(req);
        let req = self.m1.req_up(req.into());
        req
    }
    fn res_up(&self, res: S3::Res) -> S1::Res {
        let res = self.m2.res_up(res);
        let res = self.m1.res_up(res.into());
        res
    }
    fn try_req_down(&self, req: S1::Req) -> Result<S3::Req, ()> {
        let req = self.m1.try_req_down(req)?;
        let req = self.m2.try_req_down(req.try_into().map_err(|_| ())?);
        req
    }

    fn try_res_down(&self, res: <S1 as Service>::Res) -> Result<<S3 as Service>::Res, ()> {
        let res = self.m1.try_res_down(res)?;
        let res = self.m2.try_res_down(res.try_into().map_err(|_| ())?);
        res
    }
}
