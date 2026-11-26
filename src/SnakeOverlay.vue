<script setup lang="ts">
import { invoke, isTauri } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref } from 'vue'
import atlas from './ingrid-poses.png'
import { IDLE_GESTURES, idlePose, movementPose, POSES } from './ingrid'
import { CHARACTER_INSET, SnakeGame } from './snake'

type Cursor = [number, number]

interface PointerSample {
  point: Cursor | null
  restart: number
}

const POLL_INTERVAL = 1000 / 30
const SPRITE_CANVAS_SIZE = 120
const SPRITE_SCALE = 0.195
const FEET_OFFSET = 32
const IDLE_DURATION = 720
const field = ref<HTMLCanvasElement | null>(null)
const character = ref<HTMLCanvasElement | null>(null)

let fieldContext: CanvasRenderingContext2D | null = null
let characterContext: CanvasRenderingContext2D | null = null
let image: HTMLImageElement | null = null
let reducedMotion: MediaQueryList | null = null
let game: SnakeGame | null = null
let width = 0
let height = 0
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

function disk(ctx: CanvasRenderingContext2D, x: number, y: number, radius: number, color: string) {
  ctx.beginPath()
  ctx.arc(x, y, radius, 0, 2 * Math.PI)
  ctx.fillStyle = color
  ctx.fill()
}

function drawField() {
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

  for (const { x, y, radius, color } of game.food) {
    disk(ctx, x, y, radius + 2, '#292022')
    disk(ctx, x, y, radius, color)
    disk(ctx, x - radius * 0.26, y - radius * 0.28, 2, '#fff1d9')
  }
}

function drawCharacter(pose?: ReturnType<typeof movementPose>) {
  const element = character.value
  const ctx = characterContext
  if (!element || !ctx || !game || !image?.naturalWidth) return

  const { x, y } = game.head
  const middle = SPRITE_CANVAS_SIZE / 2
  element.style.transform = `translate3d(${x - middle}px, ${y - middle}px, 0)`
  ctx.clearRect(0, 0, SPRITE_CANVAS_SIZE, SPRITE_CANVAS_SIZE)

  const poseToDraw = pose ?? movementPose(game.gait, game.facing, game.gaitTime, reducedMotion?.matches)
  const [sx, sy, sw, sh] = POSES[poseToDraw.sprite]
  const fit = Math.min(1, width / (2 * CHARACTER_INSET), height / (2 * CHARACTER_INSET))
  const spriteWidth = sw * SPRITE_SCALE * fit * (poseToDraw.scaleX ?? 1)
  const spriteHeight = sh * SPRITE_SCALE * fit * (poseToDraw.scaleY ?? 1)
  const feet = FEET_OFFSET * fit
  const tilt = poseToDraw.tilt ?? 0
  const top = spriteHeight * Math.cos(tilt) + spriteWidth / 2 * Math.abs(Math.sin(tilt))
  const lift = Math.min((poseToDraw.lift ?? 0) * fit, Math.max(0, y + feet - top))

  ctx.save()
  ctx.translate(middle, middle + feet - lift)
  ctx.rotate(tilt)
  ctx.drawImage(image, sx, sy, sw, sh, -spriteWidth / 2, -spriteHeight, spriteWidth, spriteHeight)
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

function sizeCanvases() {
  const fieldElement = field.value
  const characterElement = character.value
  const ctx = fieldContext
  const sprite = characterContext
  if (!fieldElement || !characterElement || !ctx || !sprite) return
  const nextWidth = fieldElement.clientWidth
  const nextHeight = fieldElement.clientHeight
  const resized = width !== nextWidth || height !== nextHeight
  width = nextWidth
  height = nextHeight
  const scale = Math.min(window.devicePixelRatio, 2)
  fieldElement.width = Math.ceil(width * scale)
  fieldElement.height = Math.ceil(height * scale)
  characterElement.width = Math.ceil(SPRITE_CANVAS_SIZE * scale)
  characterElement.height = Math.ceil(SPRITE_CANVAS_SIZE * scale)
  ctx.setTransform(scale, 0, 0, scale, 0, 0)
  sprite.setTransform(scale, 0, 0, scale, 0, 0)
  sprite.imageSmoothingQuality = 'high'
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
  if (sample && sample.restart !== restart) {
    restart = sample.restart
    resetGame()
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
  <canvas
    ref="character"
    class="character"
    :style="{ width: SPRITE_CANVAS_SIZE + 'px', height: SPRITE_CANVAS_SIZE + 'px' }"
    aria-hidden="true"
  ></canvas>
</template>

<style>
html, body { margin: 0; overflow: hidden; background: transparent; }
canvas {
  position: fixed;
  top: 0;
  left: 0;
  pointer-events: none;
}
.playfield {
  width: 100%;
  height: 100%;
}
</style>
