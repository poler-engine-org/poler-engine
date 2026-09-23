#!/usr/bin/env python3
"""
demo_ocean.py — ПОЛИГОН «ЭФИРНЫЙ ОКЕАН»: POLER ENGINE × Panda3D.

Цикл W «Симбиоз» (v0.59.0). Вода — НЕ анимация и НЕ сеточный решатель
Panda3D: это живая спектральная гидродинамика GF(3) из libpoler_ffi.so
(тот же движок, что и `poler-engine --exec "game water"`):
  • состояние моря 1–2 КБ (2048 байт против 1 МБ f32-сетки);
  • целочисленные фазы 3^(p+m) — ноль f32-дрейфа за миллионы шагов;
  • дисперсия ω=√(gk+γk³), равновесие Пирсона–Московица.

Оптика (GLSL):
  • Френель — зеркало неба под скользящими углами;
  • солнечный блик — Blinn-Phong по аналитическим нормалям;
  • пена на гребнях — крутизна волны;
  • глубинный цвет — закон Бера–Ламберта;
  • туман к горизонту.

Кадр = ОДИН вызов C-ABI (polerf_water_geometries_f32: тик физики +
FFT-синтез + аналитические нормали) + одна memcpy в GeomVertexData.

Управление: мышь — обзор, колесо — зум, ESC — выход.
Установка: pip install panda3d numpy
Запуск:    python3 demo_ocean.py [ветер_м/с] [сетка]
Пример:    python3 demo_ocean.py 10 128
"""
import math
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])

import numpy as np

from poler_panda import Water, version

WIND = float(sys.argv[1]) if len(sys.argv) > 1 else 8.0
GRID = int(sys.argv[2]) if len(sys.argv) > 2 else 128
GRID = 1 << max(4, min(10, int(math.log2(GRID))))
DOMAIN = 140.0  # метров

from direct.showbase.ShowBase import ShowBase  # noqa: E402
from panda3d.core import (  # noqa: E402
    AmbientLight,
    DirectionalLight,
    Geom,
    GeomNode,
    GeomTriangles,
    GeomVertexData,
    GeomVertexFormat,
    TextNode,
    Vec3,
    Vec4,
    loadPrcFileData,
)

loadPrcFileData(
    "",
    """
window-title POLER ENGINE × Panda3D — Эфирный Океан (GF(3))
sync-video #t
show-frame-rate-meter #t
""",
)

# =============================================================================
# GLSL: Френель + солнце + пена + Беер–Ламберт + туман
# =============================================================================
WATER_VERTEX = """
#version 130
uniform mat4 p3d_ModelViewProjectionMatrix;

in vec4 p3d_Vertex;
in vec3 p3d_Normal;

out vec3 v_normal_w;
out vec3 v_pos_w;
out float v_steep;

void main() {
    v_normal_w = p3d_Normal;          // аналитическая нормаль спектра (CPU→GPU)
    v_pos_w = p3d_Vertex.xyz;
    v_steep = length(p3d_Normal.xz);  // 0 (плоско) .. ~1 (отвесный гребень)
    gl_Position = p3d_ModelViewProjectionMatrix * p3d_Vertex;
}
"""

WATER_FRAGMENT = """
#version 130
uniform vec3 sun_dir;    // НА солнце, world space
uniform vec3 cam_pos;    // позиция камеры, world space
uniform vec3 sky_color;
uniform vec3 deep_color;

in vec3 v_normal_w;
in vec3 v_pos_w;
in float v_steep;

out vec4 frag;

void main() {
    vec3 N = normalize(v_normal_w);
    vec3 V = normalize(cam_pos - v_pos_w);
    if (dot(N, V) < 0.0) N = -N;

    // --- Френель (Шлик) ---
    float fres = pow(1.0 - max(dot(N, V), 0.0), 5.0);
    float kr = mix(0.02, 1.0, fres);

    // --- небо в отражении ---
    vec3 R = reflect(-V, N);
    vec3 sky = mix(sky_color, vec3(1.0), pow(max(R.y, 0.0), 4.0) * 0.35);

    // --- солнечный блик (Binn-Phong, два лепестка) ---
    vec3 H = normalize(V + sun_dir);
    float ndh = max(dot(N, H), 0.0);
    float spec = pow(ndh, 260.0) * 2.4 + pow(ndh, 36.0) * 0.16;

    // --- глубинный цвет (Беер–Ламберт: гребень мелее — светлее) ---
    vec3 water = mix(deep_color, deep_color * 1.8 + vec3(0.02, 0.10, 0.11),
                     clamp(v_steep * 1.3, 0.0, 1.0));

    // --- пена на самых крутых гребнях ---
    float foam = smoothstep(0.38, 0.72, v_steep);

    vec3 col = water * (1.0 - kr) + sky * kr + vec3(1.0, 0.97, 0.90) * spec;
    col = mix(col, vec3(0.96, 0.98, 1.0), foam * 0.85);

    // --- туман к горизонту ---
    float fog = clamp(length(cam_pos - v_pos_w) / 900.0, 0.0, 0.75);
    col = mix(col, sky_color * 0.9, fog);

    frag = vec4(col, 1.0);
}
"""


class OceanApp(ShowBase):
    def __init__(self):
        super().__init__()
        self.disableMouse()

        print(f"POLER FFI : {version()}")

        # --- море POLER (вся физика — в Rust) ---
        self.sea = Water(n=GRID, modes=160, seed=42, wind=WIND, domain=DOMAIN)
        self.cell = DOMAIN / (GRID - 1)
        print(
            f"Море      : сетка {GRID}×{GRID}, домен {DOMAIN:.0f} м, "
            f"ветер {WIND} м/с → H_s = {self.sea.significant_height:.2f} м "
            f"(Пирсон–Московиц {0.21 * WIND ** 2 / 9.81:.2f})"
        )

        # --- геометрия: динамическая плоскость GRID×GRID (V3n3) ---
        fmt = GeomVertexFormat.getV3n3()
        self.vdata = GeomVertexData("poler-ocean", fmt, Geom.UHDynamic)
        self.vdata.setNumRows(GRID * GRID)

        # статичные x, y + динамичные z и нормали
        idx = np.arange(GRID * GRID, dtype=np.float32)
        self._geom = np.zeros((GRID * GRID, 6), dtype=np.float32)
        self._geom[:, 0] = (idx % GRID) * self.cell - DOMAIN / 2
        self._geom[:, 1] = (idx // GRID) * self.cell - DOMAIN / 2
        self.vdata.modifyArray(0).setData(self._geom.tobytes())

        tris = GeomTriangles(Geom.UHStatic)
        quads = np.arange(GRID * GRID, dtype=np.uint32).reshape(GRID, GRID)[:-1, :-1]
        a = quads.reshape(-1)
        b = a + 1
        c = a + GRID
        d = c + 1
        tri_idx = np.empty((a.size, 6), dtype=np.uint32)
        tri_idx[:, 0] = a
        tri_idx[:, 1] = c
        tri_idx[:, 2] = b
        tri_idx[:, 3] = b
        tri_idx[:, 4] = c
        tri_idx[:, 5] = d
        tris.setIndexType(Geom.NTUint32)
        tris.addNextVertices(a.size * 2)
        # индексный буфер одной записью (numpy → байты)
        tris.modifyVertices().modifyHandle().setData(tri_idx.tobytes())

        geom = Geom(self.vdata)
        geom.addPrimitive(tris)
        node = GeomNode("poler-ocean")
        node.addGeom(geom)
        self.ocean = self.render.attachNewNode(node)

        # --- шейдер оптики ---
        from panda3d.core import Shader

        shader = Shader.make(Shader.SL_GLSL, vertex=WATER_VERTEX, fragment=WATER_FRAGMENT)
        self.ocean.setShader(shader)
        self.ocean.setShaderInput("sun_dir", Vec3(0.45, 0.5, 0.74))
        self.ocean.setShaderInput("sky_color", Vec3(0.36, 0.55, 0.78))
        self.ocean.setShaderInput("deep_color", Vec3(0.012, 0.09, 0.14))

        # --- камера и свет ---
        self.camera.setPos(0, -55, 18)
        self.camera.setHpr(-8, -12, 0)
        self.cam_lens.setFov(62)
        self.orbit = {"h": -8.0, "p": -12.0, "r": 60.0}
        self.accept("escape", sys.exit)
        self.accept("wheel_up", self._zoom, [-6])
        self.accept("wheel_down", self._zoom, [6])
        self.taskMgr.add(self._camera_task, "camera")

        alight = AmbientLight("ambient")
        alight.setColor(Vec4(0.35, 0.38, 0.42, 1))
        self.render.setLight(self.render.attachNewNode(alight))
        dlight = DirectionalLight("sun")
        dlight.setColor(Vec4(1.0, 0.96, 0.88, 1))
        dlnp = self.render.attachNewNode(dlight)
        dlnp.setHpr(35, -55, 0)
        self.render.setLight(dlnp)

        # --- HUD ---
        self.hud = self.a2dBottomLeft.attachNewNode(TextNode("hud"))
        self.hud.node().setText("POLER × Panda3D")
        self.hud.node().setTextScale(0.045)
        self.hud.setScale(1.4)
        self.hud.setPos(0.05, 0.12)

        self.frames = 0
        self.t0 = time.perf_counter()
        self.tphys = 0.0
        self.taskMgr.add(self._physics_task, "poler-physics")

    # ------------------------------------------------------------------
    def _zoom(self, dr):
        self.orbit["r"] = max(18.0, min(240.0, self.orbit["r"] + dr))

    def _camera_task(self, task):
        md = self.mouseWatcherNode
        if md.hasMouse():
            x, y = md.getMouseX(), md.getMouseY()
            self.orbit["h"] = x * 70.0
            self.orbit["p"] = max(-72.0, min(-2.0, y * 55.0 - 8.0))
        r = self.orbit["r"]
        p = math.radians(self.orbit["p"])
        h = math.radians(self.orbit["h"])
        self.camera.setPos(
            math.sin(h) * r * math.cos(p),
            -math.cos(h) * r * math.cos(p),
            abs(math.sin(p)) * r + 4.0,
        )
        self.camera.setHpr(self.orbit["h"], self.orbit["p"], 0)
        # позиция камеры для шейдера (Френель считает виды из eye-точки)
        cp = self.camera.getPos()
        self.ocean.setShaderInput("cam_pos", cp)
        return task.cont

    def _physics_task(self, task):
        # ОДИН вызов C-ABI на кадр: тик + FFT-синтез + аналитические нормали
        t0 = time.perf_counter()
        heights, normals = self.sea.geometries(1.0 / 60.0)
        self.tphys = time.perf_counter() - t0

        n = self.sea.n
        self._geom[:, 2] = np.frombuffer(heights, dtype=np.float32)
        self._geom[:, 3:] = np.frombuffer(normals, dtype=np.float32).reshape(n * n, 3)
        self.vdata.modifyArray(0).modifyHandle().setData(self._geom.tobytes())

        self.frames += 1
        if self.frames % 30 == 0:
            now = time.perf_counter()
            fps = 30 / max(now - self.t0, 1e-9)
            self.t0 = now
            self.hud.node().setText(
                f"POLER × Panda3D | {WIND} м/с | H_s {self.sea.significant_height:.2f} м"
                f" | {fps:.0f} FPS | физика {self.tphys * 1000:.1f} мс"
                f" | hash 0x{self.sea.state_hash:08X}"
            )
        return task.cont


def main():
    app = OceanApp()
    app.run()


if __name__ == "__main__":
    main()
