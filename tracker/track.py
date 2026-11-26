"""Read the built-in camera and send eye and head features as newline-delimited JSON."""

import argparse
import json
import math
from pathlib import Path
import sys
import time

import cv2
import mediapipe as mp
from mediapipe.tasks.python import vision


WIDTH, HEIGHT, FPS = 640, 480, 24
MODEL = Path(__file__).with_name("face_landmarker.task")
# Eye corners in image order, top and bottom lid, and iris center.
EYES = ((33, 133, 159, 145, 468), (362, 263, 386, 374, 473))


def send(kind, **data):
    print(json.dumps({"type": kind, **data}, allow_nan=False), flush=True)


def create_landmarker():
    return vision.FaceLandmarker.create_from_options(
        vision.FaceLandmarkerOptions(
            base_options=mp.tasks.BaseOptions(model_asset_path=str(MODEL)),
            running_mode=vision.RunningMode.VIDEO,
            num_faces=1,
            output_facial_transformation_matrixes=True,
        )
    )


def eye_features(landmarks, indexes, width, height):
    a, b, top, bottom, iris = [
        (landmarks[index].x * width, landmarks[index].y * height)
        for index in indexes
    ]
    dx, dy = b[0] - a[0], b[1] - a[1]
    scale_squared = dx * dx + dy * dy
    if scale_squared < 36:
        return None

    def vertical(point):
        return ((point[1] - a[1]) * dx - (point[0] - a[0]) * dy) / scale_squared

    horizontal = ((iris[0] - a[0]) * dx + (iris[1] - a[1]) * dy) / scale_squared
    aperture = abs(vertical(bottom) - vertical(top))
    return horizontal, vertical(iris), aperture


def extract(result, width, height):
    if not result.face_landmarks or not result.facial_transformation_matrixes:
        return "searching", None, None, False

    rotation = result.facial_transformation_matrixes[0]
    angles = rotation[0, 2], rotation[2, 2], rotation[1, 2], rotation[1, 0], rotation[1, 1]
    if not all(math.isfinite(value) for value in angles):
        return "searching", None, None, False
    yaw = math.atan2(rotation[0, 2], rotation[2, 2])
    pitch = math.atan2(-rotation[1, 2], math.hypot(rotation[1, 0], rotation[1, 1]))
    pose = [yaw, pitch]

    face = result.face_landmarks[0]
    eyes = [eye_features(face, indexes, width, height) for indexes in EYES]
    if any(eye is None or not all(math.isfinite(value) for value in eye) for eye in eyes):
        return "searching", None, pose, False
    apertures = [eye[2] for eye in eyes]
    if min(apertures) < 0.1:
        return "eyes_closed", None, pose, max(apertures) < 0.1

    features = [(eyes[0][i] + eyes[1][i]) / 2 for i in range(2)] + pose
    return "tracked", features, pose, False


def run_camera(landmarker):
    camera = cv2.VideoCapture(0, cv2.CAP_AVFOUNDATION)
    try:
        if not camera.isOpened():
            raise RuntimeError("无法打开摄像头。请检查 macOS 的相机权限，或关闭占用相机的应用。")
        camera.set(cv2.CAP_PROP_FRAME_WIDTH, WIDTH)
        camera.set(cv2.CAP_PROP_FRAME_HEIGHT, HEIGHT)
        camera.set(cv2.CAP_PROP_FPS, FPS)
        send("ready")

        last_image_at = time.monotonic()
        while True:
            started_at = time.monotonic()
            ok, frame = camera.read()
            if not ok and started_at - last_image_at >= 2:
                raise RuntimeError("摄像头已停止提供画面。请关闭再重新开启摄像头。")
            if ok:
                last_image_at = started_at
                timestamp = time.time_ns() // 1_000_000
                if frame.shape[0] > HEIGHT or frame.shape[1] > WIDTH:
                    scale = min(WIDTH / frame.shape[1], HEIGHT / frame.shape[0])
                    frame = cv2.resize(frame, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA)
                height, width = frame.shape[:2]
                rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
                image = mp.Image(image_format=mp.ImageFormat.SRGB, data=rgb)
                result = landmarker.detect_for_video(image, int(started_at * 1_000))
                quality, features, pose, bilateral_closed = extract(result, width, height)
                send("sample", timestamp=timestamp, quality=quality, features=features, pose=pose,
                     bilateral_closed=bilateral_closed)
            time.sleep(max(0, 1 / FPS - (time.monotonic() - started_at)))
    finally:
        camera.release()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="Load the model without opening the camera.")
    args = parser.parse_args()
    try:
        if not cv2.videoio_registry.hasBackend(cv2.CAP_AVFOUNDATION):
            raise RuntimeError("OpenCV 缺少 macOS AVFoundation 摄像头支持。")
        with create_landmarker() as landmarker:
            if args.check:
                import numpy as np

                image = mp.Image(image_format=mp.ImageFormat.SRGB, data=np.zeros((HEIGHT, WIDTH, 3), dtype=np.uint8))
                landmarker.detect_for_video(image, 0)
                send("ready")
            else:
                run_camera(landmarker)
    except Exception as error:
        send("error", message=str(error))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
