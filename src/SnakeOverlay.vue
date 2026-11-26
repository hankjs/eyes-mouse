<script setup lang="ts">
import { invoke, isTauri } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref, shallowRef } from 'vue'
import atlas from './ingrid-poses.png'
import { IDLE_GESTURES, idlePose, movementPose, POSES, TAIL_ATTACHMENTS, type IdleGesture } from './ingrid'
import { DEFAULT_SNAKE_SIZE, SnakeGame, type Facing, type Food } from './snake'
import { BEAD_OUTLINE } from './tail'

type Cursor = [number, number]

interface PointerSample {
  point: Cursor | null
  restart: number
  snakeSize: number
}

interface SceneFrame {
  seconds?: number
  velocityX?: number
  velocityY?: number
  gesture?: { name: IdleGesture; progress: number }
  snap?: boolean
}

const POLL_INTERVAL = 1000 / 30
const SPRITE_CANVAS_SIZE = 120
const SPRITE_SCALE = 0.195
const FEET_OFFSET = 32
const IDLE_DURATION = 720
const POINTER_GAP = 12
const MAX_POSE_WIDTH = 68
const field = ref<HTMLCanvasElement | null>(null)
const character = ref<HTMLCanvasElement | null>(null)
const food = shallowRef<Food[]>([])

let fieldContext: CanvasRenderingContext2D | null = null
let tailBounds: [number, number, number, number] | null = null
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

function drawField() {
  const nextFood = game?.food ?? []
  const displayedFood = food.value
  if (nextFood.length !== displayedFood.length || nextFood.some((pellet, index) => pellet !== displayedFood[index])) {
    food.value = nextFood.slice()
  }
  const ctx = fieldContext
  if (!ctx) return
  if (tailBounds) ctx.clearRect(...tailBounds)
  tailBounds = null
  if (!game) return

  const { anchor, beads } = game.tail
  if (anchor) {
    let left = Infinity
    let top = Infinity
    let right = -Infinity
    let bottom = -Infinity
    const outline = BEAD_OUTLINE * anchor.scale
    ctx.lineWidth = 2 * outline
    ctx.strokeStyle = '#292022'
    for (const bead of beads) {
      const radius = bead.radius * anchor.scale
      const margin = radius + outline + 1
      left = Math.min(left, bead.x - margin)
      top = Math.min(top, bead.y - margin)
      right = Math.max(right, bead.x + margin)
      bottom = Math.max(bottom, bead.y + margin)
      ctx.beginPath()
      ctx.arc(bead.x, bead.y, radius, 0, 2 * Math.PI)
      ctx.fillStyle = bead.color
      ctx.fill()
      ctx.stroke()
      ctx.beginPath()
      ctx.arc(bead.x - 0.32 * radius, bead.y - 0.33 * radius, 0.22 * radius, 0, 2 * Math.PI)
      ctx.fillStyle = '#fff1d9'
      ctx.fill()
    }
    tailBounds = [left, top, right - left, bottom - top]
  }
}

function drawScene({ seconds = 0, velocityX = 0, velocityY = 0, gesture, snap = false }: SceneFrame = {}) {
  const element = character.value
  const ctx = characterContext
  if (!element || !ctx || !game || !image?.naturalWidth || !characterCanvasSize) {
    drawField()
    return false
  }

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

  const [anchorX, anchorY] = TAIL_ATTACHMENTS[poseToDraw.sprite]
  const u = (anchorX - sx) / sw
  const v = (anchorY - sy) / sh
  const localX = ((poseToDraw.sprite === 'idle' && displayFacing === -1 ? 1 - u : u) - 0.5) * spriteWidth
  const localY = (v - 1) * spriteHeight
  const back = Math.atan2(0.55, -displayFacing) + tilt
  const angle = reduced ? back : Math.atan2(Math.sin(back) * 145 - velocityY * 0.48, Math.cos(back) * 145 - velocityX * 0.34)
  const movingTail = game.tail.follow({
    x: drawX + localX * cos - localY * sin,
    y: feetY + localX * sin + localY * cos,
    angle,
    scale: characterScale,
  }, seconds, snap || reduced)
  drawField()
  return movingTail || (!constrainedOffset && horizontalOffset !== desiredOffset)
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
  tailBounds = null
  ctx.setTransform(scale, 0, 0, scale, 0, 0)
  sizeCharacterCanvas()
  if (resized) resetGame()
  else drawScene({ snap: true })
}

function animateMovement(now: number) {
  movementFrame = 0
  if (!game) return
  const head = game.head
  const seconds = Math.min(0.05, Math.max(0, (now - previousFrame) / 1000))
  const moving = game.advance(seconds)
  previousFrame = now
  const settling = drawScene({
    seconds,
    velocityX: seconds ? (game.head.x - head.x) / seconds : 0,
    velocityY: seconds ? (game.head.y - head.y) / seconds : 0,
  })
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
