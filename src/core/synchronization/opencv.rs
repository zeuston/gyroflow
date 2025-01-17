// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use nalgebra::Rotation3;
use std::ffi::c_void;
use std::sync::Arc;
use opencv::core::{ Mat, Size, Point2f, TermCriteria, CV_8UC1 };
use opencv::prelude::MatTraitConst;
use super::{ EstimatorItem, EstimatorItemInterface, OpticalFlowPair };

use crate::stabilization::ComputeParams;

// use opencv::prelude::{PlatformInfoTraitConst, DeviceTraitConst, UMatTraitConst};
// use opencv::core::{UMat, UMatUsageFlags, AccessFlag::ACCESS_READ};

#[derive(Default, Clone)]
pub struct ItemOpenCV {
    features: Vec<(f32, f32)>,
    img: Arc<image::GrayImage>,
    size: (i32, i32)
}

impl EstimatorItemInterface for ItemOpenCV {
    fn get_features(&self) -> &Vec<(f32, f32)> {
        &self.features
    }

    fn estimate_pose(&self, next: &EstimatorItem, params: &ComputeParams, timestamp_us: i64, next_timestamp_us: i64) -> Option<Rotation3<f64>> {
        let (pts1, pts2) = self.get_matched_features(next)?;

        let result = || -> Result<Rotation3<f64>, opencv::Error> {
            let pts11 = crate::stabilization::undistort_points_for_optical_flow(&pts1, timestamp_us, params, (self.img.width(), self.img.height()));
            let pts22 = crate::stabilization::undistort_points_for_optical_flow(&pts2, next_timestamp_us, params, (self.img.width(), self.img.height()));

            let pts1 = pts11.into_iter().map(|(x, y)| Point2f::new(x, y)).collect::<Vec<Point2f>>();
            let pts2 = pts22.into_iter().map(|(x, y)| Point2f::new(x, y)).collect::<Vec<Point2f>>();

            let a1_pts = Mat::from_slice(&pts1)?;
            let a2_pts = Mat::from_slice(&pts2)?;

            let identity = Mat::eye(3, 3, opencv::core::CV_64F)?;

            let mut mask = Mat::default();
            let e = opencv::calib3d::find_essential_mat(&a1_pts, &a2_pts, &identity, opencv::calib3d::RANSAC, 0.999, 0.0005, 1000, &mut mask)?;

            let mut r1 = Mat::default();
            let mut t = Mat::default();

            let inliers = opencv::calib3d::recover_pose_triangulated(&e, &a1_pts, &a2_pts, &identity, &mut r1, &mut t, 100000.0, &mut mask, &mut Mat::default())?;
            if inliers < 20 {
                return Err(opencv::Error::new(0, "Model not found".to_string()));
            }

            cv_to_rot2(r1)
        }();

        match result {
            Ok(res) => Some(res),
            Err(e) => {
                log::error!("OpenCV error: {:?}", e);
                None
            }
        }
    }

    fn optical_flow_to(&self, to: &EstimatorItem) -> OpticalFlowPair {
        self.get_matched_features(to)
    }
    fn cleanup(&mut self) {
        self.img = Arc::new(image::GrayImage::default());
    }
}

impl ItemOpenCV {
    pub fn detect_features(_timestamp_us: i64, img: Arc<image::GrayImage>, width: u32, height: u32) -> Self {
        let (w, h) = (width as i32, height as i32);
        let inp = unsafe { Mat::new_size_with_data(Size::new(w, h), CV_8UC1, img.as_raw().as_ptr() as *mut c_void, img.width() as usize) };

        // opencv::imgcodecs::imwrite("D:/test.jpg", &inp, &opencv::types::VectorOfi32::new());

        let mut pts = Mat::default();

        //let inp = inp.get_umat(ACCESS_READ, UMatUsageFlags::USAGE_DEFAULT).unwrap();
        //let mut pts = UMat::new(UMatUsageFlags::USAGE_DEFAULT);

        if let Err(e) = inp.and_then(|inp| {
            opencv::imgproc::good_features_to_track(&inp, &mut pts, 200, 0.01, 10.0, &Mat::default(), 3, false, 0.04)
        }) {
            log::error!("OpenCV error {:?}", e);
        }

        //let pts = pts.get_mat(ACCESS_READ).unwrap().clone();
        Self {
            features: (0..pts.rows()).into_iter().filter_map(|i| { let x = pts.at::<Point2f>(i).ok()?; Some((x.x, x.y))}).collect(),
            size: (w, h),
            img
        }
    }

    fn get_matched_features(&self, next: &EstimatorItem) -> Option<(Vec<(f32, f32)>, Vec<(f32, f32)>)> {
        if let EstimatorItem::ItemOpenCV(next) = next {
            let (w, h) = self.size;
            if self.img.is_empty() || next.img.is_empty() || w <= 0 || h <= 0 { return None; }

            let result = || -> Result<(Vec<(f32, f32)>, Vec<(f32, f32)>), opencv::Error> {
                let a1_img = unsafe { Mat::new_size_with_data(Size::new(w, h), CV_8UC1, self.img.as_raw().as_ptr() as *mut c_void, w as usize) }?;
                let a2_img = unsafe { Mat::new_size_with_data(Size::new(w, h), CV_8UC1, next.img.as_raw().as_ptr() as *mut c_void, w as usize) }?;

                let pts1: Vec<Point2f> = self.features.iter().map(|(x, y)| Point2f::new(*x as f32, *y as f32)).collect();

                let a1_pts = Mat::from_slice(&pts1)?;
                //let a2_pts = a2.features;

                let mut a2_pts = Mat::default();
                let mut status = Mat::default();
                let mut err = Mat::default();

                opencv::video::calc_optical_flow_pyr_lk(&a1_img, &a2_img, &a1_pts, &mut a2_pts, &mut status, &mut err, Size::new(21, 21), 3, TermCriteria::new(3/*count+eps*/,30,0.01)?, 0, 1e-4)?;

                let mut pts1 = Vec::with_capacity(status.rows() as usize);
                let mut pts2 = Vec::with_capacity(status.rows() as usize);
                for i in 0..status.rows() {
                    if *status.at::<u8>(i)? == 1u8 {
                        let pt1 = a1_pts.at::<Point2f>(i)?;
                        let pt2 = a2_pts.at::<Point2f>(i)?;
                        if pt1.x >= 0.0 && pt1.x < w as f32 && pt1.y >= 0.0 && pt1.y < h as f32
                        && pt2.x >= 0.0 && pt2.x < w as f32 && pt2.y >= 0.0 && pt2.y < h as f32 {
                            pts1.push((pt1.x as f32, pt1.y as f32));
                            pts2.push((pt2.x as f32, pt2.y as f32));
                        }
                    }
                }
                Ok((pts1, pts2))
            }();

            match result {
                Ok(res) => Some(res),
                Err(e) => {
                    log::error!("OpenCV error: {:?}", e);
                    None
                }
            }
        } else {
            None
        }
    }
}

pub fn init() -> Result<(), opencv::Error> {
    /*use opencv::prelude::DeviceTraitConst;
    use opencv::prelude::PlatformInfoTraitConst;
    let opencl_have = opencv::core::have_opencl()?;
    if opencl_have {
        opencv::core::set_use_opencl(true)?;
        let mut platforms = opencv::types::VectorOfPlatformInfo::new();
        opencv::core::get_platfoms_info(&mut platforms)?;
        for (platf_num, platform) in platforms.into_iter().enumerate() {
            ::log::info!("Platform #{}: {}", platf_num, platform.name()?);
            for dev_num in 0..platform.device_number()? {
                let mut dev = opencv::core::Device::default();
                platform.get_device(&mut dev, dev_num)?;
                ::log::info!("  OpenCL device #{}: {}", dev_num, dev.name()?);
                ::log::info!("    vendor:  {}", dev.vendor_name()?);
                ::log::info!("    version: {}", dev.version()?);
            }
        }
    }
    let opencl_use = opencv::core::use_opencl()?;
    ::log::info!(
        "OpenCL is {} and {}",
        if opencl_have { "available" } else { "not available" },
        if opencl_use { "enabled" } else { "disabled" },
    );*/
    Ok(())
}

fn cv_to_rot2(r1: Mat) -> Result<Rotation3<f64>, opencv::Error> {
    if r1.typ() != opencv::core::CV_64FC1 {
        return Err(opencv::Error::new(0, "Invalid matrix type".to_string()));
    }
    Ok(Rotation3::from_matrix_unchecked(nalgebra::Matrix3::new(
        *r1.at_2d::<f64>(0, 0)?, *r1.at_2d::<f64>(0, 1)?, *r1.at_2d::<f64>(0, 2)?,
        *r1.at_2d::<f64>(1, 0)?, *r1.at_2d::<f64>(1, 1)?, *r1.at_2d::<f64>(1, 2)?,
        *r1.at_2d::<f64>(2, 0)?, *r1.at_2d::<f64>(2, 1)?, *r1.at_2d::<f64>(2, 2)?
    )))
}
