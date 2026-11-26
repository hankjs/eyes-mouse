<script setup lang="ts">
import { invoke, isTauri } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref, shallowRef } from 'vue'
import atlas from './ingrid-poses.png'
import { IDLE_GESTURES, idlePose, movementPose, POSES } from './ingrid'
import { DEFAULT_SNAKE_SIZE, SnakeGame, type Food } from './snake'

type Cursor = [number, number]

interface PointerSample {
  point: Cursor | null
  restart: number
  snakeSize: number
}

const POLL_INTERVAL = 1000 / 30
const SPRITE_CANVAS_SIZE = 120
const SPRITE_SCALE = 0.195
const FEET_OFFSET = 32
const IDLE_DURATION = 720
const field = ref<HTMLCanvasElement | null>(null)
const character = ref<HTMLCanvasElement | null>(null)
const food = shallowRef<Food[]>([])

let fieldContext: CanvasRenderingContext2D | null = null
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
let restart = 0
let polling = false
let readingPointer = false
let pollSession = 0
let pollTimer: number | undefined

function drawField() {
  const nextFood = game?.food ?? []
  const displayedFood = food.value
  if (nextFood.length !== displayedFood.length || nextFood.some((pellet, index) => pellet !== displayedFood[index])) {
    food.value = nextFood.slice()
  }
  const ctx = fieldContext
  if (!ctx || !game) return
  ctx.clearRect(0, 0, width, height)

  const { trail } = game
  if (trail.length > 1) {
    ctx.beginPath()
    ctx.moveTo(trail[0].x, trail[0].y)
    for (let i = 1; i < trail.length; i++) ctx.lineTo(trail[i].x, trail[i].y)
    ctx.lineCap = 'round'
    ctx.lineJoin = 'round'
    ctx.lineWidth = 12
    ctx.strokeStyle = 'rgba(99, 124, 185, 0.48)'
    ctx.stroke()
  }
}

function drawCharacter(pose?: ReturnType<typeof movementPose>) {
  const element = character.value
  const ctx = characterContext
  if (!element || !ctx || !game || !image?.naturalWidth || !characterCanvasSize) return

  const { x, y } = game.head
  const middle = characterCanvasSize / 2
  const poseToDraw = pose ?? movementPose(game.gait, game.facing, game.gaitTime, reducedMotion?.matches)
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

  // Keep the whole pose in view while the game head can still reach food at the edges.
  const drawX = Math.min(width - right, Math.max(left, x))
  const feetY = Math.min(height - bottom, Math.max(top, y + feet - lift))
  element.style.transform = `translate3d(${drawX - middle}px, ${feetY - feet - middle}px, 0)`
  ctx.clearRect(0, 0, characterCanvasSize, characterCanvasSize)
  ctx.save()
  ctx.translate(middle, middle + feet)
  ctx.rotate(tilt)
  ctx.drawImage(image, sx, sy, sw, sh, -halfWidth, -spriteHeight, spriteWidth, spriteHeight)
  ctx.restore()
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
  drawCharacter()
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
  nextGesture = (nextGesture + 1) % IDLE_GESTURES.length
  const animate = (now: number) => {
    idleFrame = 0
    if (!game) return
    const progress = (now - start) / IDLE_DURATION
    if (progress >= 1) {
      drawCharacter()
      scheduleIdle()
      return
    }
    drawCharacter(idlePose(gesture, game.facing, progress))
    idleFrame = requestAnimationFrame(animate)
  }
  idleFrame = requestAnimationFrame(animate)
}

function resetGame() {
  cancelMovement()
  cancelIdle()
  cursor = null
  nextGesture = 0
  game = width && height ? new SnakeGame(width, height) : null
  drawField()
  drawCharacter()
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
  ctx.setTransform(scale, 0, 0, scale, 0, 0)
  sizeCharacterCanvas()
  if (resized) resetGame()
  else {
    drawField()
    drawCharacter()
  }
}

function animateMovement(now: number) {
  movementFrame = 0
  if (!game) return
  const head = game.head
  const moving = game.advance((now - previousFrame) / 1000)
  previousFrame = now
  if (head !== game.head) drawField()
  drawCharacter()
  if (moving) movementFrame = requestAnimationFrame(animateMovement)
  else scheduleIdle()
}

function steer(point: Cursor | null) {
  if (point?.[0] === cursor?.[0] && point?.[1] === cursor?.[1]) return
  cursor = point
  if (!point) {
    pause()
    scheduleIdle()
    return
  }
  if (!game) return
  if (!game.setTarget({ x: point[0], y: point[1] })) {
    if (movementFrame) drawCharacter()
    cancelMovement()
    scheduleIdle()
    return
  }
  cancelIdle()
  if (!movementFrame) {
    previousFrame = performance.now()
    movementFrame = requestAnimationFrame(animateMovement)
  }
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
      drawCharacter()
    }
  }
  steer(sample?.point ?? null)
  // Wait for each native reply before scheduling another read.
  const interval = sample ? POLL_INTERVAL : 100
  pollTimer = window.setTimeout(() => pollPointer(session), Math.max(0, Math.ceil(interval - (performance.now() - started))))
}

function stopPolling() {
  polling = false
  pollSession++
  window.clearTimeout(pollTimer)
  cursor = null
  pause()
}

function updatePolling() {
  if (document.hidden) stopPolling()
  else if (!polling) {
    polling = true
    void pollPointer(++pollSession)
    scheduleIdle()
  }
}

function motionPreferenceChanged() {
  cancelIdle()
  drawCharacter()
  scheduleIdle()
}

onMounted(() => {
  fieldContext = field.value?.getContext('2d') ?? null
  characterContext = character.value?.getContext('2d') ?? null
  if (!fieldContext || !characterContext) return
  reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)')
  reducedMotion.addEventListener('change', motionPreferenceChanged)
  image = new Image()
  image.onload = () => drawCharacter()
  image.src = atlas
  sizeCanvases()
  window.addEventListener('resize', sizeCanvases)
  if (!isTauri()) return
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
  <span v-for="{ x, y, radius, color } in food" :key="`${x},${y}`" class="pellet" aria-hidden="true"
    :style="{ left: `${x}px`, top: `${y}px`, '--pellet-diameter': `${2 * radius}px`, backgroundColor: color }"></span>
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
}
.pellet {
  box-sizing: content-box;
  width: var(--pellet-diameter);
  height: var(--pellet-diameter);
  border: 2px solid #292022;
  border-radius: 50%;
  translate: -50% -50%;
}
.pellet::after {
  content: '';
  position: absolute;
  left: 37%;
  top: 36%;
  width: 4px;
  height: 4px;
  border-radius: 50%;
  background: #fff1d9;
  translate: -50% -50%;
}
</style>
