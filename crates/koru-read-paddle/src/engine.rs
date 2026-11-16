// SPDX-License-Identifier: MPL-2.0

//! **The pipeline itself**: `oar-ocr`'s detector and recognizer on ONNX
//! Runtime, built with the settings in [`crate::settings`]. Compiled only with
//! `runtime`, on a target with a pinned ONNX Runtime library.
//!
//! Plan 0.1's throwaway measurement program built exactly this pipeline and
//! read every synthetic fixture with it at a character error rate of 0.000.

use koru_read::{FailureCode, Picture, ReadError, Reader, ReaderInfo, Recognized};
use oar_ocr::core::config::OrtSessionConfig;
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{OAROCR, OAROCRBuilder};
use oar_ocr::processors::LimitType;

use crate::lines::{self, Region};
use crate::model::ModelDir;
use crate::settings::{DETECTION, REGION_BATCH, Settings};

/// The reader. Owns both ONNX Runtime sessions and the thread pool their pre-
/// and post-processing runs on.
pub struct PaddleReader {
    ocr: OAROCR,
    pool: rayon::ThreadPool,
}

fn needs_reader(message: impl std::fmt::Display) -> ReadError {
    ReadError {
        code: FailureCode::NeedsReader,
        message: message.to_string(),
    }
}

fn internal(message: impl std::fmt::Display) -> ReadError {
    ReadError {
        code: FailureCode::Internal,
        message: message.to_string(),
    }
}

impl PaddleReader {
    /// Build the pipeline from `model`, whose dictionary has already been
    /// checked and read (`dictionary`).
    pub fn load(
        model: &ModelDir,
        dictionary: String,
        settings: &Settings,
    ) -> Result<PaddleReader, ReadError> {
        let threads = settings.threads();
        // **Bound every rayon pool in the process.** `oar-ocr` crops, resizes
        // and post-processes on the global pool, and the image decoder uses it
        // too; left alone it is one thread per core, and a backlog of scans
        // would take the whole machine. The global pool can be set only once
        // per process and only before first use, so the pipeline's own calls
        // also run inside a pool of their own (`read`), which holds whatever
        // happened first.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("koru-read-pool-{index}"))
            .build_global();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("koru-read-ocr-{index}"))
            .build()
            .map_err(internal)?;

        let detection = TextDetectionConfig {
            score_threshold: DETECTION.score_threshold,
            box_threshold: DETECTION.box_threshold,
            unclip_ratio: DETECTION.unclip_ratio,
            max_candidates: DETECTION.max_candidates,
            limit_side_len: Some(DETECTION.max_side),
            limit_type: Some(LimitType::Max),
            ..TextDetectionConfig::default()
        };
        let session = OrtSessionConfig::default()
            .with_intra_threads(threads)
            .with_inter_threads(1);
        let ocr = OAROCRBuilder::new(model.detector(), model.recognizer(), model.dictionary())
            .character_dict_content(dictionary)
            .text_detection_config(detection)
            .ort_session(session)
            .image_batch_size(1)
            .region_batch_size(REGION_BATCH)
            .build()
            .map_err(needs_reader)?;
        Ok(PaddleReader { ocr, pool })
    }
}

impl Reader for PaddleReader {
    fn describe(&self) -> ReaderInfo {
        ReaderInfo {
            reader: crate::reader_id(),
            version: crate::VERSION.to_string(),
        }
    }

    fn read(&mut self, picture: &Picture<'_>) -> Result<Recognized, ReadError> {
        let image =
            image::RgbImage::from_raw(picture.width(), picture.height(), picture.pixels().to_vec())
                .ok_or_else(|| internal("the picture's pixels do not match its size"))?;
        let ocr = &self.ocr;
        let results = self
            .pool
            .install(|| ocr.predict(vec![image]))
            .map_err(internal)?;
        let regions = results
            .into_iter()
            .next()
            .map(|result| {
                result
                    .text_regions
                    .into_iter()
                    .filter_map(|region| {
                        let text = region.text?;
                        let bounds = &region.bounding_box;
                        Some(Region {
                            left: bounds.x_min(),
                            top: bounds.y_min(),
                            right: bounds.x_max(),
                            bottom: bounds.y_max(),
                            text: text.to_string(),
                            confidence: region.confidence,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(lines::assemble(regions))
    }
}
