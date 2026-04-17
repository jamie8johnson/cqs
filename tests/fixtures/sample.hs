module Sample where

-- | A 2D point
data Point = Point { x :: Double, y :: Double }

-- | Color enumeration
data Color = Red | Green | Blue deriving (Show, Eq)

-- | Type alias for a name
type Name = String

-- | Printable typeclass
class Printable a where
  display :: a -> String

-- | Instance of Printable for Color
instance Printable Color where
  display Red   = "Red"
  display Green = "Green"
  display Blue  = "Blue"

-- | Add two integers
add :: Int -> Int -> Int
add a b = a + b

-- | Greet a person
greet :: String -> String
greet name = "Hello, " ++ name

-- | Process a list of items
process :: [String] -> IO ()
process items = do
  let filtered = filter (not . null) items
  mapM_ putStrLn filtered
  print (length filtered)

-- | Calculate distance between two points
distance :: Point -> Point -> Double
distance p1 p2 = sqrt (dx * dx + dy * dy)
  where
    dx = x p1 - x p2
    dy = y p1 - y p2

-- | A newtype wrapper
newtype UserId = UserId Int
