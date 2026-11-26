<script setup lang="ts">
import { invoke, isTauri } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref, shallowRef } from 'vue'
import atlas from './ingrid-poses.png'
import { IDLE_GESTURES, idlePose, movementPose, POSES, type IdleGesture } from './ingrid'
import { drawLightEffects, type PaintBounds } from './light-effects'
import type { LightAnchor } from './lights'
import { DEFAULT_SNAKE_SIZE, SnakeGame, type Facing, type Food } from './snake'

type Cursor = [number, number]

interface PointerSample {
  point: Cursor | null
  restart: number
  snakeSize: number
}

interface SceneFrame {
  seconds?: number
  moving?: boolean
  gesture?: { name: IdleGesture; progress: number }
  snap?: boolean
}

const POLL_INTERVAL = 1000 / 30
const LIGHT_FRAME_INTERVAL = 1000 / 30
const SPRITE_CANVAS_SIZE = 120
const SPRITE_SCALE = 0.195
const FEET_OFFSET = 32
const IDLE_DURATION = 720
const POINTER_GAP = 12
const MAX_POSE_WIDTH = 68
const field = ref<HTMLCanvasElement | null>(null)
const character = ref<HTMLCanvasElement | null>(null)
const food = shallowRef<Food[]>([])
const animationsPaused = ref(false)

let fieldContext: CanvasRenderingContext2D | null = null
let lightBounds: PaintBounds[] = []
let lightAnchor: LightAnchor | null = null
let lightMoving = false
let lightFrame = 0
let previousLightFrame = 0
let lightTime = 0
let characterContext: CanvasRenderingContext2D | null = null
let image: HTMLImageElement | null = null
let reducedMotion: MediaQueryList | null = null
let game: SnakeGame | null = null
let width = 0
let height = 0
let snakeSize = isTauri() ? null : DEFAULT_SNAKE_SIZE
let characterScale = 0
let characterCanvasSize = 0
let movementFrame = 0
let previousFrame = 0
let idleFrame = 0
let idleTimer: number | undefined
let nextGesture = 0
let cursor: Cursor | null = null
let pointerX: number | null = null
let displayFacing: Facing = 1
let horizontalOffset = 0
let restart = 0
let polling = false
let readingPointer = false
let pollSession = 0
let pollTimer: number | undefined

function updateFood() {
  const nextFood = game?.food ?? []
  const displayedFood = food.value
  if (nextFood.length !== displayedFood.length || nextFood.some((pellet, index) => pellet !== displayedFood[index])) {
    food.value = nextFood.slice()
  }
}

function paintLights(seconds: number, snap = false) {
  const ctx = fieldContext
  if (!ctx || !game || !lightAnchor || animationsPaused.value || document.hidden) return false
  const reduced = reducedMotion?.matches ?? false
  const active = game.orbit.follow(lightAnchor, seconds, { moving: lightMoving, reduced, snap })
  if (!reduced) lightTime += seconds
  for (const area of lightBounds) ctx.clearRect(...area)
  lightBounds = drawLightEffects(ctx, game.orbit, lightTime, reduced, Math.min(width, height) * 0.18)
  return active
}

function cancelLights() {
  if (lightFrame) cancelAnimationFrame(lightFrame)
  lightFrame = 0
  previousLightFrame = 0
}

function scheduleLights() {
  if (lightFrame || !lightAnchor || animationsPaused.value || document.hidden) return
  if (!previousLightFrame) previousLightFrame = performance.now()
  lightFrame = requestAnimationFrame(animateLights)
}

function animateLights(now: number) {
  lightFrame = 0
  const elapsed = now - previousLightFrame
  if (elapsed < LIGHT_FRAME_INTERVAL - 1) {
    scheduleLights()
    return
  }
  previousLightFrame = now
  if (paintLights(elapsed / 1000)) scheduleLights()
  else previousLightFrame = 0
}

function drawScene({ seconds = 0, moving = false, gesture, snap = false }: SceneFrame = {}) {
  updateFood()
  const element = character.value
  const ctx = characterContext
  if (!element || !ctx || !game || !image?.naturalWidth || !characterCanvasSize) return false

  const { x, y } = game.head
  const middle = characterCanvasSize / 2
  const reduced = reducedMotion?.matches ?? false
  if (pointerX !== null) {
    const wantedFacing = Math.abs(pointerX - x) > POINTER_GAP ? game.facing : displayFacing
    const preferred = wantedFacing === 1 ? pointerX : width - pointerX
    const opposite = width - preferred
    const needed = MAX_POSE_WIDTH * characterScale + POINTER_GAP
    const buffer = displayFacing === wantedFacing ? 0 : 6
    displayFacing = preferred >= needed + buffer || preferred >= opposite ? wantedFacing : wantedFacing === 1 ? -1 : 1
  }
  const poseToDraw = gesture ? idlePose(gesture.name, displayFacing, gesture.progress)
    : movementPose(game.gait, displayFacing, game.gaitTime, reduced)
  const [sx, sy, sw, sh] = POSES[poseToDraw.sprite]
  const spriteWidth = sw * SPRITE_SCALE * characterScale * (poseToDraw.scaleX ?? 1)
  const spriteHeight = sh * SPRITE_SCALE * characterScale * (poseToDraw.scaleY ?? 1)
  const feet = FEET_OFFSET * characterScale
  const halfWidth = spriteWidth / 2
  const tilt = poseToDraw.tilt ?? 0
  const cos = Math.cos(tilt)
  const sin = Math.sin(tilt)
  const bottom = halfWidth * Math.abs(sin)
  const top = spriteHeight * cos + bottom
  const left = halfWidth * cos + Math.max(0, -spriteHeight * sin)
  const right = halfWidth * cos + Math.max(0, spriteHeight * sin)
  const lift = (poseToDraw.lift ?? 0) * characterScale
  const desiredOffset = pointerX === null ? 0 : displayFacing === 1 ? -right - POINTER_GAP : left + POINTER_GAP
  if (snap || reduced || Math.abs(desiredOffset - horizontalOffset) < 0.25) horizontalOffset = desiredOffset
  else horizontalOffset += (desiredOffset - horizontalOffset) * (1 - Math.exp(-24 * seconds))

  // Keep the full sprite on screen while its logical head can reach food under the pointer.
  let drawX = Math.min(width - right, Math.max(left, x + horizontalOffset))
  const feetY = Math.min(height - bottom, Math.max(top, y + feet - lift))
  let constrainedOffset = false
  if (cursor && cursor[1] >= feetY - top && cursor[1] <= feetY + bottom) {
    const leftOfCursor = cursor[0] - POINTER_GAP - right
    const rightOfCursor = cursor[0] + POINTER_GAP + left
    if (drawX > leftOfCursor && drawX < rightOfCursor) {
      const canStayLeft = leftOfCursor >= left
      const canStayRight = rightOfCursor <= width - right
      if (canStayLeft && (displayFacing === 1 || !canStayRight)) drawX = leftOfCursor
      else if (canStayRight) drawX = rightOfCursor
      if (canStayLeft || canStayRight) {
        horizontalOffset = drawX - x
        constrainedOffset = true
      }
    }
  }
  element.style.transform = `translate3d(${drawX - middle}px, ${feetY - feet - middle}px, 0)`
  ctx.clearRect(0, 0, characterCanvasSize, characterCanvasSize)
  ctx.save()
  ctx.translate(middle, middle + feet)
  ctx.rotate(tilt)
  ctx.drawImage(image, sx, sy, sw, sh, -halfWidth, -spriteHeight, spriteWidth, spriteHeight)
  ctx.restore()

  const settling = !constrainedOffset && horizontalOffset !== desiredOffset
  const bodyHeight = 0.42 * spriteHeight
  lightAnchor = {
    x: drawX + bodyHeight * sin,
    y: feetY - bodyHeight * cos,
    scale: characterScale,
    radiusX: Math.max(26 * characterScale, 0.43 * spriteWidth),
    radiusY: Math.max(16 * characterScale, 0.29 * spriteHeight),
  }
  lightMoving = moving || settling
  if (snap) {
    cancelLights()
    if (paintLights(0, true)) scheduleLights()
  } else {
    scheduleLights()
  }
  return settling
}

function cancelIdle() {
  window.clearTimeout(idleTimer)
  idleTimer = undefined
  if (idleFrame) cancelAnimationFrame(idleFrame)
  idleFrame = 0
}

function cancelMovement() {
  if (movementFrame) cancelAnimationFrame(movementFrame)
  movementFrame = 0
}

function pause() {
  game?.setTarget(null)
  cancelMovement()
  cancelIdle()
  drawScene({ snap: true })
}

function scheduleIdle() {
  if (!game || !polling || movementFrame || reducedMotion?.matches || idleTimer !== undefined || idleFrame) return
  idleTimer = window.setTimeout(startGesture, 1850 + nextGesture * 430)
}

function startGesture() {
  idleTimer = undefined
  if (!game || !polling || reducedMotion?.matches || document.hidden) return
  const gesture = IDLE_GESTURES[nextGesture]
  const start = performance.now()
  let previous = start
  nextGesture = (nextGesture + 1) % IDLE_GESTURES.length
  const animate = (now: number) => {
    idleFrame = 0
    if (!game) return
    const progress = (now - start) / IDLE_DURATION
    const seconds = Math.min(0.05, Math.max(0, (now - previous) / 1000))
    previous = now
    if (progress >= 1) {
      if (drawScene({ seconds })) startMovement()
      else scheduleIdle()
      return
    }
    drawScene({ seconds, gesture: { name: gesture, progress } })
    idleFrame = requestAnimationFrame(animate)
  }
  idleFrame = requestAnimationFrame(animate)
}

function resetGame() {
  cancelMovement()
  cancelIdle()
  cancelLights()
  lightAnchor = null
  lightTime = 0
  cursor = null
  pointerX = null
  displayFacing = 1
  horizontalOffset = 0
  nextGesture = 0
  game = width && height ? new SnakeGame(width, height, Math.random, window.screen.width || width) : null
  drawScene({ snap: true })
  scheduleIdle()
}

function sizeCharacterCanvas() {
  const element = character.value
  const ctx = characterContext
  if (!element || !ctx || snakeSize === null) return
  characterScale = Math.min(snakeSize / 100, width / SPRITE_CANVAS_SIZE, height / SPRITE_CANVAS_SIZE)
  characterCanvasSize = SPRITE_CANVAS_SIZE * characterScale
  const pixels = Math.ceil(characterCanvasSize * Math.min(window.devicePixelRatio, 2))
  element.width = pixels
  element.height = pixels
  element.style.width = `${characterCanvasSize}px`
  element.style.height = `${characterCanvasSize}px`
  if (!pixels) return
  const scale = pixels / characterCanvasSize
  ctx.setTransform(scale, 0, 0, scale, 0, 0)
  ctx.imageSmoothingQuality = 'high'
}

function sizeCanvases() {
  const fieldElement = field.value
  const ctx = fieldContext
  if (!fieldElement || !ctx) return
  const nextWidth = fieldElement.clientWidth
  const nextHeight = fieldElement.clientHeight
  const resized = width !== nextWidth || height !== nextHeight
  width = nextWidth
  height = nextHeight
  const scale = Math.min(window.devicePixelRatio, 2)
  fieldElement.width = Math.ceil(width * scale)
  fieldElement.height = Math.ceil(height * scale)
  lightBounds = []
  ctx.setTransform(scale, 0, 0, scale, 0, 0)
  sizeCharacterCanvas()
  if (resized) resetGame()
  else drawScene({ snap: true })
}

function animateMovement(now: number) {
  movementFrame = 0
  if (!game) return
  const seconds = Math.min(0.05, Math.max(0, (now - previousFrame) / 1000))
  const moving = game.advance(seconds)
  previousFrame = now
  const settling = drawScene({ seconds, moving })
  if (moving || settling) movementFrame = requestAnimationFrame(animateMovement)
  else scheduleIdle()
}

function startMovement() {
  if (movementFrame) return
  previousFrame = performance.now()
  movementFrame = requestAnimationFrame(animateMovement)
}

function steer(point: Cursor | null) {
  if (point?.[0] === cursor?.[0] && point?.[1] === cursor?.[1]) return
  cursor = point
  if (!game) return
  if (point) pointerX = point[0]
  game.setTarget(point ? { x: point[0], y: point[1] } : null)
  cancelIdle()
  startMovement()
}

async function pollPointer(session: number) {
  if (readingPointer || session !== pollSession) return
  readingPointer = true
  const started = performance.now()
  let sample: PointerSample | null = null
  try {
    sample = await invoke<PointerSample | null>('snake_pointer')
  } catch {
    // Treat a missed sample as a pointer outside the play area.
  }
  readingPointer = false
  if (session !== pollSession) {
    if (polling) void pollPointer(pollSession)
    return
  }
  if (sample) {
    const nextSize = sample.snakeSize ?? DEFAULT_SNAKE_SIZE
    const resized = nextSize !== snakeSize
    if (resized) {
      snakeSize = nextSize
      sizeCharacterCanvas()
    }
    if (sample.restart !== restart) {
      restart = sample.restart
      resetGame()
    } else if (resized) {
      drawScene({ snap: true })
    }
  }
  steer(sample?.point ?? null)
  // Wait for each native reply before scheduling another read.
  const interval = sample ? POLL_INTERVAL : 100
  pollTimer = window.setTimeout(() => pollPointer(session), Math.max(0, Math.ceil(interval - (performance.now() - started))))
}

function stopPolling() {
  animationsPaused.value = true
  cancelLights()
  polling = false
  pollSession++
  window.clearTimeout(pollTimer)
  cursor = null
  pause()
}

function updatePolling() {
  if (document.hidden) {
    stopPolling()
    return
  }
  if (animationsPaused.value) {
    animationsPaused.value = false
    if (paintLights(0, true)) scheduleLights()
  }
  if (isTauri() && !polling) {
    polling = true
    void pollPointer(++pollSession)
    scheduleIdle()
  }
}

function motionPreferenceChanged() {
  cancelIdle()
  drawScene({ snap: true })
  scheduleIdle()
}

onMounted(() => {
  fieldContext = field.value?.getContext('2d') ?? null
  characterContext = character.value?.getContext('2d') ?? null
  if (!fieldContext || !characterContext) return
  reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)')
  reducedMotion.addEventListener('change', motionPreferenceChanged)
  image = new Image()
  image.onload = () => drawScene({ snap: true })
  image.src = atlas
  sizeCanvases()
  window.addEventListener('resize', sizeCanvases)
  document.addEventListener('visibilitychange', updatePolling)
  window.addEventListener('pagehide', stopPolling)
  window.addEventListener('pageshow', updatePolling)
  updatePolling()
})

onUnmounted(() => {
  stopPolling()
  if (image) image.onload = null
  reducedMotion?.removeEventListener('change', motionPreferenceChanged)
  window.removeEventListener('resize', sizeCanvases)
  document.removeEventListener('visibilitychange', updatePolling)
  window.removeEventListener('pagehide', stopPolling)
  window.removeEventListener('pageshow', updatePolling)
})
</script>

<template>
  <canvas ref="field" class="playfield" aria-hidden="true"></canvas>
  <canvas ref="character" class="character" aria-hidden="true"></canvas>
  <!-- Pellets stay visible when their positions overlap the character. -->
  <span v-for="{ x, y, radius, tier } in food" :key="`${x},${y},${tier}`" class="pellet" aria-hidden="true"
    :class="{ paused: animationsPaused }" :data-tier="tier"
    :style="{ left: `${x}px`, top: `${y}px`, '--pellet-diameter': `${2 * radius}px` }"></span>
</template>

<style>
html, body { margin: 0; overflow: hidden; background: transparent; }
canvas, .pellet {
  position: fixed;
  pointer-events: none;
}
canvas {
  top: 0;
  left: 0;
}
.playfield {
  width: 100%;
  height: 100%;
  z-index: 1;
}
.pellet {
  --twinkle-delay: -0.6s;
  z-index: 2;
  width: var(--pellet-diameter);
  height: var(--pellet-diameter);
  border-radius: 50%;
  translate: -50% -50%;
  background: radial-gradient(circle, #ffffec 18%, #fff4ab 27%, #ffce43 43%, #e89b14 55%, #b5741d 62%, transparent 69%);
}
.pellet[data-tier="1"] {
  --twinkle-delay: -1.2s;
}
.pellet[data-tier="2"] {
  --twinkle-delay: -1.7s;
}
.pellet::before,
.pellet::after {
  content: '';
  position: absolute;
  animation: twinkle 1.25s ease-in-out var(--twinkle-delay) infinite alternate;
}
.pellet::before {
  inset: -32%;
  border-radius: 50%;
  background: radial-gradient(circle, transparent 35%, #eca32259 47%, #c9721326 58%, transparent 71%);
}
.pellet::after {
  inset: -4% 10%;
  background: #fff8d6;
  clip-path: polygon(50% 0, 60% 40%, 100% 50%, 60% 60%, 50% 100%, 40% 60%, 0 50%, 40% 40%);
}
.pellet.paused::before,
.pellet.paused::after {
  animation-play-state: paused;
}
@keyframes twinkle {
  from { opacity: 0.66; transform: scale(0.87); }
  to { opacity: 1; transform: scale(1); }
}
@media (prefers-reduced-motion: reduce) {
  .pellet::before,
  .pellet::after { animation: none; }
}
</style>
