// Diet tracking data — rewritten on each food/exercise log
// Dashboard-Fancy.html loads this via <script src="diet-today.js">
// Reload the HTML page in browser to pick up changes
window.DIET_TODAY = {
  date: "2026-10-06",
  dayStyle: "normal",
  dayType: "PHASE 2 CUT. Tuesday October 6, home. Base 1700 kcal from diet-logs/calorie-base.csv.",
  weight: null,
  exercise: [],
  meals: [
    { name: "Breakfast", time: "07:20", items: [
      { item: "Americano (black, no milk or sugar)", amount: "1 cup", cal: 3, p: 0.1, f: 0.1, c: 0.5, fiber: 0, na: 4, satf: 0, sug: 0, k: 35, ca: 1, o3: 0, mg: 24, chol: 0, tfat: 0, asug: 0, pur: 0, hg: 0, se: 0, vd: 0, caf: 63, iod: null, fe: 0, ret: null, ox: null, o6: 0, alc: 0, cat: "soft_drink", src: "home", basis: "reference", tsrc: "actual" }
    ]},
    { name: "Breakfast", time: "07:25", items: [
      { item: "Oatmeal with blueberries", amount: "40g oats, 80g blueberries, water", cal: 198, p: 6.3, f: 2.9, c: 38, fiber: 6.2, na: 3, satf: 0.5, sug: 8.5, k: 190, ca: 28, o3: null, mg: 57, chol: 0, tfat: 0, asug: 0, pur: null, hg: 0, se: 11, vd: 0, caf: 0, iod: null, fe: 1.9, ret: null, ox: null, o6: 1, alc: 0, cat: "food", src: "home", basis: "weighed", tsrc: "actual" }
    ]}
  ],
  targets: { calories: 1700, protein: 140, fat: 65, carbs: 180, carbsBase: 139, fiber: 38, sodium: 2000, satFat: 22 },
  rolling7: {
    days: 7, from: "2026-09-30", to: "2026-10-06",
    nutrients: {
      mercury_ug: { known: 0, knownCount: 6, unknownCount: 0 },
      omega3_mg: { known: 0, knownCount: 1, unknownCount: 5 }
    }
  }
};
